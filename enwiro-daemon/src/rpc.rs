//! JSON-RPC 2.0 server over a unix domain socket via jsonrpsee 0.26.
//!
//! See ADR-0002. Plugin↔host data emission stays on stdout-JSONL; this
//! module is only the client↔daemon channel.
//!
//! Wire: newline-delimited JSON-RPC. We bypass jsonrpsee's
//! `Server`/`serve_with_graceful_shutdown` helpers (HTTP-shaped, would
//! pull in `hyper`+`tower`) and dispatch raw JSON-RPC frames via
//! `RpcModule::raw_json_request`. Net result: zero hyper/tower in the
//! daemon's dep tree, and `socat -u UNIX-CONNECT:/run/.../rpc.sock - | jq .`
//! consumes the wire natively.
//!
//! Per-connection state is currently none (no subscriptions yet); ADR
//! Layer 3 will add a connection-scoped notification channel later.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use async_trait::async_trait;
use enwiro_sdk::rpc::{
    APPLICATION_ERROR_CODE, CALL_CHAIN_ENV_VAR, CYCLE_DETECTED_CODE, CookbookInvokeParams,
    CookbookInvokeResult, EnwiroRpcServer,
};
use futures_util::{SinkExt, StreamExt};
use jsonrpsee::core::server::Methods;
use jsonrpsee::types::{ErrorObjectOwned, error::ErrorCode};
use tokio::net::{UnixListener, UnixStream};
use tokio_util::codec::{Framed, LinesCodec};

/// Newline-delimited JSON frame limit (1 MiB). Mirrors the client-side
/// cap; bounds memory against pathological peer behaviour.
const MAX_FRAME_BYTES: usize = 1024 * 1024;

/// Subscription buffer per connection (deferred — no subscriptions
/// today). 16 is a safe placeholder for `raw_json_request`'s buf_size.
const SUBSCRIPTION_BUF: usize = 16;

/// Shared daemon-side state. Empty for now; ADR-0002 reserves room for
/// `last_activated_env` (env.current Layer 2) and an event broadcaster
/// (Layer 3). Both out of scope for this branch.
#[derive(Default)]
pub struct State {}

/// The struct that implements the RPC trait. Holds an `Arc<State>` so
/// handlers can read shared state without per-request allocations.
#[derive(Clone)]
struct DaemonRpc {
    #[allow(dead_code)]
    state: Arc<State>,
}

#[async_trait]
impl EnwiroRpcServer for DaemonRpc {
    async fn cookbook_invoke(
        &self,
        params: CookbookInvokeParams,
    ) -> Result<CookbookInvokeResult, ErrorObjectOwned> {
        let start = std::time::Instant::now();
        tracing::info!(
            cookbook = %params.cookbook,
            op = %params.op,
            chain = ?params.call_chain,
            "cookbook.invoke dispatched"
        );

        // Cycle detection — the call_chain conventionally arrives via
        // the SDK helper, which forwards `$ENWIRO_RPC_CALL_CHAIN` from
        // the calling cookbook's env. ADR-0002 §4.
        if params.call_chain.contains(&params.cookbook) {
            return Err(ErrorObjectOwned::owned(
                CYCLE_DETECTED_CODE,
                format!(
                    "cycle in cookbook.invoke: chain {:?} would reinvoke {}",
                    params.call_chain, params.cookbook
                ),
                Some(serde_json::json!({
                    "chain": params.call_chain,
                    "cookbook": params.cookbook,
                })),
            ));
        }

        // Locate the cookbook plugin by name. `get_plugins` walks PATH
        // for `enwiro-cookbook-*` executables (existing behaviour).
        let plugin = enwiro_sdk::plugin::get_plugins(enwiro_sdk::plugin::PluginKind::Cookbook)
            .into_iter()
            .find(|p| p.name == params.cookbook)
            .ok_or_else(|| {
                ErrorObjectOwned::owned::<()>(
                    APPLICATION_ERROR_CODE,
                    format!("cookbook '{}' not found", params.cookbook),
                    None,
                )
            })?;

        tracing::debug!(
            cookbook = %params.cookbook,
            executable = %plugin.executable,
            op = %params.op,
            "spawning cookbook subprocess"
        );

        // Args: positional strings (today's cookbook protocol is `cook <recipe>`
        // / `list-recipes` / `gear <recipe>`). Structured params are a future
        // extension of the cookbook contract, not the RPC.
        let positional_args = positional_args_from_value(&params.args);

        // Chain forwarded into the spawned child so transitive
        // cookbook.invoke calls see the lineage.
        let mut extended_chain = params.call_chain.clone();
        extended_chain.push(params.cookbook.clone());
        let chain_env_value = extended_chain.join(":");

        let cookbook_name = params.cookbook.clone();
        let op_for_spawn = params.op.clone();
        let executable = plugin.executable.clone();
        let payload = enwiro_sdk::CookbookPayload::new(params.payload.clone());

        let output =
            tokio::task::spawn_blocking(move || -> std::io::Result<std::process::Output> {
                use std::io::Write;
                let mut child = std::process::Command::new(&executable)
                    .arg(&op_for_spawn)
                    .args(&positional_args)
                    .env(CALL_CHAIN_ENV_VAR, &chain_env_value)
                    .stdin(std::process::Stdio::piped())
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .spawn()?;
                if let Some(mut stdin) = child.stdin.take() {
                    let bytes = serde_json::to_vec(&payload)
                        .map_err(|e| std::io::Error::other(format!("serialise payload: {}", e)))?;
                    stdin.write_all(&bytes)?;
                }
                child.wait_with_output()
            })
            .await
            .map_err(|e| {
                ErrorObjectOwned::owned::<()>(
                    ErrorCode::InternalError.code(),
                    format!("spawn task join failed: {}", e),
                    None,
                )
            })?
            .map_err(|e| {
                ErrorObjectOwned::owned::<()>(
                    ErrorCode::InternalError.code(),
                    format!("cookbook spawn failed: {}", e),
                    None,
                )
            })?;

        let elapsed = start.elapsed();
        if !output.status.success() {
            tracing::warn!(
                cookbook = %cookbook_name,
                op = %params.op,
                exit_code = ?output.status.code(),
                elapsed_ms = elapsed.as_millis() as u64,
                "cookbook exited with non-zero status"
            );
            return Err(ErrorObjectOwned::owned(
                APPLICATION_ERROR_CODE,
                format!(
                    "cookbook '{}' op '{}' exited with non-zero status",
                    cookbook_name, params.op
                ),
                Some(serde_json::json!({
                    "stderr": String::from_utf8_lossy(&output.stderr).to_string(),
                    "exit_code": output.status.code(),
                })),
            ));
        }

        let stdout = String::from_utf8(output.stdout).map_err(|e| {
            ErrorObjectOwned::owned::<()>(
                APPLICATION_ERROR_CODE,
                format!("cookbook '{}' produced invalid UTF-8: {}", cookbook_name, e),
                None,
            )
        })?;
        tracing::info!(
            cookbook = %cookbook_name,
            op = %params.op,
            elapsed_ms = elapsed.as_millis() as u64,
            stdout_bytes = stdout.len(),
            "cookbook.invoke completed"
        );

        Ok(CookbookInvokeResult { v: 1, stdout })
    }
}

fn positional_args_from_value(args: &serde_json::Value) -> Vec<String> {
    match args {
        serde_json::Value::Array(items) => items
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect(),
        _ => Vec::new(),
    }
}

/// Bind the unix socket, set perms to 0600, and run the accept loop.
pub async fn serve(socket_path: PathBuf, state: Arc<State>) -> anyhow::Result<()> {
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create runtime dir {}", parent.display()))?;
    }
    let _ = std::fs::remove_file(&socket_path);
    let listener = UnixListener::bind(&socket_path)
        .with_context(|| format!("bind unix socket at {}", socket_path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("chmod 0600 {}", socket_path.display()))?;
    }
    serve_listener(listener, state, socket_path).await
}

/// Run the accept loop against a pre-bound listener. Exposed so tests
/// can own bind themselves (and surface bind errors directly) before
/// signalling readiness.
pub async fn serve_listener(
    listener: UnixListener,
    state: Arc<State>,
    socket_path: PathBuf,
) -> anyhow::Result<()> {
    let methods: Methods = DaemonRpc { state }.into_rpc().into();
    tracing::info!(path = %socket_path.display(), "rpc server listening");

    loop {
        let (stream, _addr) = match listener.accept().await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "rpc accept failed");
                continue;
            }
        };
        let methods = methods.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_conn(stream, methods).await {
                tracing::warn!(error = %e, "rpc connection closed with error");
            }
        });
    }
}

/// Per-connection loop: read one JSON-RPC frame per line, dispatch via
/// `Methods::raw_json_request`, write the response back as one line.
async fn handle_conn(stream: UnixStream, methods: Methods) -> anyhow::Result<()> {
    let mut framed = Framed::new(stream, LinesCodec::new_with_max_length(MAX_FRAME_BYTES));
    while let Some(line) = framed.next().await {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                tracing::debug!(error = %e, "rpc framing error; closing connection");
                break;
            }
        };
        // jsonrpsee owns the protocol: parse, dispatch, build response.
        let response_str = match methods.raw_json_request(&line, SUBSCRIPTION_BUF).await {
            Ok((response, _subscription_rx)) => response.get().to_owned(),
            Err(e) => {
                // Only triggers on `serde_json::from_str` failure for the
                // outer envelope shape. Surface as a JSON-RPC parse error
                // with id: null.
                build_parse_error_line(&e)
            }
        };
        if let Err(e) = framed.send(response_str).await {
            tracing::debug!(error = %e, "rpc response write failed; closing connection");
            break;
        }
    }
    Ok(())
}

/// Build a JSON-RPC parse-error response with `id: null`. Matches the
/// JSON-RPC 2.0 spec: when the request can't be parsed at all, the
/// server replies with `{"jsonrpc":"2.0","id":null,"error":{"code":-32700,...}}`.
fn build_parse_error_line(e: &serde_json::Error) -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": serde_json::Value::Null,
        "error": {
            "code": ErrorCode::ParseError.code(),
            "message": format!("parse error: {}", e),
        },
    })
    .to_string()
}

/// Default socket path under `$XDG_RUNTIME_DIR/enwiro/`.
pub fn default_socket_path() -> anyhow::Result<PathBuf> {
    enwiro_sdk::rpc::default_socket_path()
}

/// Return the path the daemon should advertise in `ENWIRO_RPC_SOCKET`.
pub fn socket_env_value(socket_path: &Path) -> String {
    socket_path.to_string_lossy().into_owned()
}
