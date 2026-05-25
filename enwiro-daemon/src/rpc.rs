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

use std::path::PathBuf;

use crate::meta::{
    CookedPhase, EventLogEntry, EventType, Status, load_env_meta, now_utc, save_env_meta,
};
use anyhow::Context;
use async_trait::async_trait;
use enwiro_sdk::rpc::{
    APPLICATION_ERROR_CODE, CALL_CHAIN_ENV_VAR, CYCLE_DETECTED_CODE, CookbookInvokeParams,
    CookbookInvokeResult, EnvCurrentResult, EnvMarkParams, EnvMarkResult, EnwiroRpcServer,
};
use futures_util::{SinkExt, StreamExt};
use jsonrpsee::core::server::Methods;
use jsonrpsee::types::{ErrorObjectOwned, error::ErrorCode};
use std::sync::{Arc, Mutex};
use tokio::net::{UnixListener, UnixStream};
use tokio_util::codec::{Framed, LinesCodec};

/// Server-side framing limit. Bounds memory against pathological peer
/// behaviour. Matches the client's limit by convention but is not part
/// of the wire contract — each side caps its own input.
const MAX_FRAME_BYTES: usize = 1024 * 1024;

/// Per-invocation cookbook stdout cap (16 MiB). Bounds daemon memory
/// against a cookbook that dumps an entire build log; if exceeded, we
/// kill the cookbook and return APPLICATION_ERROR with an explicit
/// "stdout truncated" message.
const MAX_INVOKE_STDOUT_BYTES: u64 = 16 * 1024 * 1024;

/// Per-invocation cookbook stderr cap (1 MiB). Stderr is only surfaced
/// in error payloads; truncate aggressively.
const MAX_INVOKE_STDERR_BYTES: u64 = 1024 * 1024;

pub struct ActiveEnvState {
    pub env_name: String,
    pub timestamp: i64,
}

pub type SharedActiveEnv = Arc<Mutex<Option<ActiveEnvState>>>;

#[derive(Clone)]
struct DaemonRpc {
    active_env: SharedActiveEnv,
    workspaces_directory: PathBuf,
}

/// `APPLICATION_ERROR_CODE` constructor — every "cookbook X failed at Y"
/// error in this module funnels through here so the wire shape stays
/// consistent.
fn app_err(message: impl Into<String>) -> ErrorObjectOwned {
    ErrorObjectOwned::owned::<()>(APPLICATION_ERROR_CODE, message.into(), None)
}

fn app_err_with_data(message: impl Into<String>, data: serde_json::Value) -> ErrorObjectOwned {
    ErrorObjectOwned::owned(APPLICATION_ERROR_CODE, message.into(), Some(data))
}

#[async_trait]
impl EnwiroRpcServer for DaemonRpc {
    #[tracing::instrument(skip(self, params), fields(cookbook = %params.cookbook, op = %params.op))]
    async fn cookbook_invoke(
        &self,
        params: CookbookInvokeParams,
    ) -> Result<CookbookInvokeResult, ErrorObjectOwned> {
        let start = std::time::Instant::now();
        tracing::info!(chain = ?params.call_chain, "cookbook.invoke dispatched");

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
            .ok_or_else(|| app_err(format!("cookbook '{}' not found", params.cookbook)))?;

        tracing::debug!(executable = %plugin.executable, "spawning cookbook subprocess");

        // Chain forwarded into the spawned child so transitive
        // cookbook.invoke calls see the lineage.
        let mut extended_chain = params.call_chain.clone();
        extended_chain.push(params.cookbook.clone());
        let chain_env_value = extended_chain.join(":");

        let payload = enwiro_sdk::CookbookPayload::new(params.payload.clone());

        // tokio::process gives us async pipes + kill_on_drop: if the
        // handler returns early (client disconnect, task cancellation),
        // the cookbook child is killed automatically rather than leaked.
        // Bounded reads on stdout / stderr cap daemon memory against a
        // cookbook that dumps a gigabyte of build output.
        let output = run_cookbook_subprocess(
            &plugin.executable,
            &params.op,
            &params.args,
            &chain_env_value,
            &payload,
        )
        .await
        .map_err(|e| {
            ErrorObjectOwned::owned::<()>(
                ErrorCode::InternalError.code(),
                format!("cookbook spawn failed: {e}"),
                None,
            )
        })?;

        let stderr_text = String::from_utf8_lossy(&output.stderr).into_owned();
        let elapsed_ms = start.elapsed().as_millis() as u64;

        if output.stdout_truncated {
            tracing::warn!(
                "cookbook stdout exceeded MAX_INVOKE_STDOUT_BYTES; killed and truncated"
            );
            return Err(app_err_with_data(
                format!(
                    "cookbook '{}' op '{}' produced more than {} bytes of stdout (truncated and killed)",
                    params.cookbook, params.op, MAX_INVOKE_STDOUT_BYTES
                ),
                serde_json::json!({
                    "max_stdout_bytes": MAX_INVOKE_STDOUT_BYTES,
                    "stderr": stderr_text,
                }),
            ));
        }

        if !output.status.success() {
            tracing::warn!(
                exit_code = ?output.status.code(),
                elapsed_ms,
                stderr = %stderr_text,
                "cookbook exited with non-zero status"
            );
            return Err(app_err_with_data(
                format!(
                    "cookbook '{}' op '{}' exited with non-zero status",
                    params.cookbook, params.op
                ),
                serde_json::json!({
                    "stderr": stderr_text,
                    "exit_code": output.status.code(),
                }),
            ));
        }

        let stdout = String::from_utf8(output.stdout).map_err(|e| {
            app_err(format!(
                "cookbook '{}' produced invalid UTF-8: {e}",
                params.cookbook
            ))
        })?;
        tracing::info!(
            elapsed_ms,
            stdout_bytes = stdout.len(),
            "cookbook.invoke completed"
        );

        Ok(CookbookInvokeResult { stdout })
    }

    async fn env_current(&self) -> Result<EnvCurrentResult, ErrorObjectOwned> {
        let state = self.active_env.lock().unwrap();
        Ok(EnvCurrentResult {
            env_name: state.as_ref().map(|s| s.env_name.clone()),
            timestamp: state.as_ref().map(|s| {
                chrono::DateTime::from_timestamp(s.timestamp, 0)
                    .map(|dt| dt.to_rfc3339())
                    .unwrap_or_else(|| s.timestamp.to_string())
            }),
        })
    }

    async fn env_mark(&self, params: EnvMarkParams) -> Result<EnvMarkResult, ErrorObjectOwned> {
        let env_dir = self.workspaces_directory.join(&params.env_name);
        if !env_dir.is_dir() {
            return Err(app_err(format!(
                "environment directory does not exist: {}",
                env_dir.display()
            )));
        }

        let new_status = match params.status.as_str() {
            "ready" => Status::Cooked {
                phase: None,
                detail: None,
            },
            "active" => Status::Cooked {
                phase: Some(CookedPhase::Active),
                detail: None,
            },
            "waiting" => Status::Cooked {
                phase: Some(CookedPhase::Waiting),
                detail: None,
            },
            "done" => Status::Done { outcome: None },
            "evergreen" => Status::Evergreen,
            other => return Err(app_err(format!("unknown status: {other}"))),
        };

        let now = now_utc();
        let mut meta = load_env_meta(&env_dir);
        meta.status = Some(new_status);
        meta.event_log.push(EventLogEntry {
            event_type: EventType::StatusChange,
            detail: params.status.clone(),
            started: now,
            ended: Some(now),
        });

        save_env_meta(&env_dir, &meta)
            .map_err(|e| app_err(format!("could not save metadata: {e}")))?;

        Ok(EnvMarkResult { ok: true })
    }
}

/// Output of a single cookbook subprocess invocation; carries enough
/// for the caller to decide between "ok", "non-zero exit", and "blew
/// past stdout cap so we killed it".
struct CookbookOutput {
    status: std::process::ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    stdout_truncated: bool,
}

/// Spawn a cookbook child, pipe `CookbookPayload` to its stdin, read
/// stdout + stderr concurrently with hard caps, wait for exit.
///
/// `kill_on_drop(true)` on the child guarantees that if this future is
/// cancelled (e.g. the RPC client disconnected mid-call), the cookbook
/// process receives SIGKILL instead of leaking.
async fn run_cookbook_subprocess(
    executable: &str,
    op: &str,
    positional_args: &[String],
    chain_env_value: &str,
    payload: &enwiro_sdk::CookbookPayload,
) -> std::io::Result<CookbookOutput> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut child = tokio::process::Command::new(executable)
        .arg(op)
        .args(positional_args)
        .env(CALL_CHAIN_ENV_VAR, chain_env_value)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;

    // Write payload to stdin and close, so the cookbook sees EOF on its
    // payload read. Done inside its own scope so `stdin` drops (closing
    // the pipe) before we await stdout/stderr — otherwise a cookbook
    // that blocks reading payload would deadlock us.
    {
        let mut stdin = child
            .stdin
            .take()
            .expect("piped stdin must be present after spawn");
        let bytes = serde_json::to_vec(payload)
            .map_err(|e| std::io::Error::other(format!("serialise cookbook payload: {}", e)))?;
        stdin.write_all(&bytes).await?;
    }

    let mut stdout_pipe = child
        .stdout
        .take()
        .expect("piped stdout must be present after spawn");
    let mut stderr_pipe = child
        .stderr
        .take()
        .expect("piped stderr must be present after spawn");

    // Cap stdout / stderr reads at one byte beyond their respective
    // caps; if the resulting buffer is exactly cap+1 we know more data
    // was waiting and the cookbook is producing too much.
    let stdout_limit = MAX_INVOKE_STDOUT_BYTES + 1;
    let stderr_limit = MAX_INVOKE_STDERR_BYTES + 1;

    let mut stdout_buf: Vec<u8> = Vec::new();
    let mut stderr_buf: Vec<u8> = Vec::new();

    // Bind the `.take(n)` adapters to locals so they outlive the
    // `read_to_end` borrow; otherwise the temporary returned by
    // `.take()` drops at the end of the macro expansion.
    let mut stdout_capped = (&mut stdout_pipe).take(stdout_limit);
    let mut stderr_capped = (&mut stderr_pipe).take(stderr_limit);
    let (stdout_res, stderr_res) = tokio::join!(
        stdout_capped.read_to_end(&mut stdout_buf),
        stderr_capped.read_to_end(&mut stderr_buf),
    );
    stdout_res?;
    stderr_res?;

    let stdout_truncated = stdout_buf.len() as u64 > MAX_INVOKE_STDOUT_BYTES;
    if stdout_truncated {
        // Cookbook is producing too much — kill it now rather than wait
        // for it to finish writing megabytes more into a pipe we won't
        // read.
        let _ = child.kill().await;
    }
    stdout_buf.truncate(MAX_INVOKE_STDOUT_BYTES as usize);
    stderr_buf.truncate(MAX_INVOKE_STDERR_BYTES as usize);

    let status = child.wait().await?;
    Ok(CookbookOutput {
        status,
        stdout: stdout_buf,
        stderr: stderr_buf,
        stdout_truncated,
    })
}

/// Bind the unix socket, set perms to 0600, and run the accept loop.
pub async fn serve(
    socket_path: PathBuf,
    active_env: SharedActiveEnv,
    workspaces_directory: PathBuf,
) -> anyhow::Result<()> {
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
    serve_listener(listener, socket_path, active_env, workspaces_directory).await
}

/// Run the accept loop against a pre-bound listener. Exposed so tests
/// can own bind themselves (and surface bind errors directly) before
/// signalling readiness.
pub async fn serve_listener(
    listener: UnixListener,
    socket_path: PathBuf,
    active_env: SharedActiveEnv,
    workspaces_directory: PathBuf,
) -> anyhow::Result<()> {
    let rpc = DaemonRpc {
        active_env,
        workspaces_directory,
    };
    let methods: Methods = rpc.into_rpc().into();
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
        // The `16` is `raw_json_request`'s buf_size param for subscription
        // notifications; we have none yet, so any positive value works.
        let response_str = match methods.raw_json_request(&line, 16).await {
            Ok((response, _subscription_rx)) => response.get().to_owned(),
            Err(e) => serde_json::json!({
                "jsonrpc": "2.0",
                "id": serde_json::Value::Null,
                "error": {
                    "code": ErrorCode::ParseError.code(),
                    "message": format!("parse error: {}", e),
                },
            })
            .to_string(),
        };
        if let Err(e) = framed.send(response_str).await {
            tracing::debug!(error = %e, "rpc response write failed; closing connection");
            break;
        }
    }
    Ok(())
}
