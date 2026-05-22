//! JSON-RPC 2.0 server over unix domain socket.
//!
//! See ADR-0002. Plugin↔host data emission stays on stdout-JSONL; this
//! module is only the client↔daemon channel.
//!
//! Each accepted connection runs its own tokio task; the OS multiplexes
//! sockets and tokio multiplexes tasks, so multi-client support is free.
//! Per-connection state is none for now (no subscriptions yet); future
//! Layer 3 work will add a `ConnCtx`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use enwiro_sdk::rpc::{
    CALL_CHAIN_ENV_VAR, CookbookInvokeParams, CookbookInvokeResult, MAX_FRAME_BYTES, Request,
    RequestEnvelope, ResponseEnvelope, RpcError,
};
use futures_util::{SinkExt, StreamExt};
use tokio::net::{UnixListener, UnixStream};
use tokio_util::codec::{Framed, LinesCodec};

/// Server-side shared state. Currently nothing; ADR-0002 reserves slots
/// for `last_activated_env` (Layer 2 `env.current` work) and the event
/// broadcaster (Layer 3). Both are out of scope for this prototype.
#[derive(Default)]
pub struct State {}

/// Serve forever. Accepts on `socket_path`, spawns one task per connection.
/// Unlinks any stale socket file first; sets `0600` perms after bind.
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
    serve_listener(listener, state, socket_path).await;
    Ok(())
}

/// Run the accept loop against a pre-bound listener. Exposed so tests can
/// own bind themselves (and surface bind errors directly) before signalling
/// readiness. Production callers use `serve` instead.
pub async fn serve_listener(listener: UnixListener, state: Arc<State>, socket_path: PathBuf) {
    tracing::info!(path = %socket_path.display(), "rpc server listening");
    loop {
        let (stream, _addr) = match listener.accept().await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "rpc accept failed");
                continue;
            }
        };
        let state = state.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_conn(stream, state).await {
                tracing::warn!(error = %e, "rpc connection closed with error");
            }
        });
    }
}

async fn handle_conn(stream: UnixStream, state: Arc<State>) -> anyhow::Result<()> {
    let mut framed = Framed::new(stream, LinesCodec::new_with_max_length(MAX_FRAME_BYTES));
    while let Some(line) = framed.next().await {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                tracing::debug!(error = %e, "rpc framing error; closing connection");
                break;
            }
        };
        let resp = dispatch(&line, &state).await;
        let serialised = serde_json::to_string(&resp)?;
        framed.send(serialised).await?;
    }
    Ok(())
}

/// Parse one request line, route to the matching handler, build a response
/// envelope. Never panics — parse failures yield a JSON-RPC error response
/// with `id = 0` (best-effort; the spec allows `null` but our envelope
/// requires a u64).
async fn dispatch(line: &str, state: &Arc<State>) -> ResponseEnvelope {
    let envelope: RequestEnvelope = match serde_json::from_str(line) {
        Ok(e) => e,
        Err(e) => return ResponseEnvelope::parse_error(format!("parse error: {}", e)),
    };

    let id = envelope.id;
    let result = match envelope.request {
        Request::CookbookInvoke(params) => cookbook_invoke(params, state).await,
    };

    match result {
        Ok(value) => ResponseEnvelope::ok(id, value),
        Err(error) => ResponseEnvelope::err(id, error),
    }
}

/// `cookbook.invoke` handler — delegate a cookbook op via the daemon.
///
/// Resolution: look up the cookbook plugin by name via `enwiro_sdk::plugin`,
/// spawn it with the existing stdout-JSONL contract, capture stdout,
/// return verbatim. Cycle detection via `call_chain`: if `cookbook` already
/// appears in the chain we refuse to extend.
async fn cookbook_invoke(
    params: CookbookInvokeParams,
    _state: &Arc<State>,
) -> Result<serde_json::Value, RpcError> {
    let start = std::time::Instant::now();
    tracing::info!(
        cookbook = %params.cookbook,
        op = %params.op,
        chain = ?params.call_chain,
        "cookbook.invoke dispatched"
    );
    if params.call_chain.contains(&params.cookbook) {
        return Err(RpcError {
            code: RpcError::CYCLE_DETECTED,
            message: format!(
                "cycle in cookbook.invoke: chain {:?} would reinvoke {}",
                params.call_chain, params.cookbook
            ),
            data: Some(serde_json::json!({
                "chain": params.call_chain,
                "cookbook": params.cookbook,
            })),
        });
    }

    let plugin = enwiro_sdk::plugin::get_plugins(enwiro_sdk::plugin::PluginKind::Cookbook)
        .into_iter()
        .find(|p| p.name == params.cookbook)
        .ok_or_else(|| RpcError {
            code: RpcError::APPLICATION_ERROR,
            message: format!("cookbook '{}' not found", params.cookbook),
            data: None,
        })?;
    tracing::debug!(
        cookbook = %params.cookbook,
        executable = %plugin.executable,
        op = %params.op,
        "spawning cookbook subprocess"
    );

    let mut extended_chain = params.call_chain.clone();
    extended_chain.push(params.cookbook.clone());
    let chain_env_value = extended_chain.join(":");

    // Args are conveyed as positional CLI args after the op name when they
    // are simple strings (matches today's `cookbook cook <recipe>` shape);
    // structured args are out of scope for this prototype and reserved
    // for a follow-up extension of the cookbook protocol.
    let positional_args = positional_args_from_value(&params.args);

    let cookbook_name = params.cookbook.clone();
    let op = params.op.clone();
    let op_for_spawn = op.clone();
    let executable = plugin.executable.clone();
    let payload = enwiro_sdk::CookbookPayload::new(params.payload.clone());
    let output = tokio::task::spawn_blocking(move || -> std::io::Result<std::process::Output> {
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
            // closing stdin happens on drop
        }
        child.wait_with_output()
    })
    .await
    .map_err(|e| RpcError {
        code: RpcError::INTERNAL_ERROR,
        message: format!("spawn task join failed: {}", e),
        data: None,
    })?
    .map_err(|e| RpcError {
        code: RpcError::INTERNAL_ERROR,
        message: format!("cookbook spawn failed: {}", e),
        data: None,
    })?;

    let elapsed = start.elapsed();
    if !output.status.success() {
        tracing::warn!(
            cookbook = %cookbook_name,
            op = %op,
            exit_code = ?output.status.code(),
            elapsed_ms = elapsed.as_millis() as u64,
            "cookbook exited with non-zero status"
        );
        return Err(RpcError {
            code: RpcError::APPLICATION_ERROR,
            message: format!(
                "cookbook '{}' op '{}' exited with non-zero status",
                cookbook_name, op
            ),
            data: Some(serde_json::json!({
                "stderr": String::from_utf8_lossy(&output.stderr).to_string(),
                "exit_code": output.status.code(),
            })),
        });
    }

    let stdout = String::from_utf8(output.stdout).map_err(|e| RpcError {
        code: RpcError::APPLICATION_ERROR,
        message: format!("cookbook '{}' produced invalid UTF-8: {}", cookbook_name, e),
        data: None,
    })?;
    tracing::info!(
        cookbook = %cookbook_name,
        op = %op,
        elapsed_ms = elapsed.as_millis() as u64,
        stdout_bytes = stdout.len(),
        "cookbook.invoke completed"
    );

    let result = CookbookInvokeResult { v: 1, stdout };
    serde_json::to_value(&result).map_err(|e| RpcError {
        code: RpcError::INTERNAL_ERROR,
        message: format!("serialise result: {}", e),
        data: None,
    })
}

/// Best-effort: if `args` is an array of strings, return them as a Vec;
/// otherwise return an empty vec. Structured params per-op shape is a
/// follow-up; today's cookbook protocol takes positional args (e.g.
/// `cook <recipe>`).
fn positional_args_from_value(args: &serde_json::Value) -> Vec<String> {
    match args {
        serde_json::Value::Array(items) => items
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect(),
        _ => Vec::new(),
    }
}

/// Convenience: default socket path under `$XDG_RUNTIME_DIR/enwiro/`.
pub fn default_socket_path() -> anyhow::Result<PathBuf> {
    enwiro_sdk::rpc::default_socket_path()
}

/// Return the path the daemon should advertise in `ENWIRO_RPC_SOCKET`.
pub fn socket_env_value(socket_path: &Path) -> String {
    socket_path.to_string_lossy().into_owned()
}
