//! Synchronous JSON-RPC client over a unix domain socket.
//!
//! One request in flight at a time per `Client`; `&mut self` enforces it at
//! compile time. Concurrent in-flight calls aren't needed by today's
//! consumers (`enw` makes one or two calls per invocation) and dropping
//! that capability eliminates the entire pending-request-routing /
//! background-reader-task stack.

use crate::rpc::{
    CookbookInvokeParams, CookbookInvokeResult, MAX_FRAME_BYTES, Request, RequestEnvelope,
    ResponseBody, ResponseEnvelope, RpcError, default_socket_path,
};
use futures_util::{SinkExt, StreamExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::net::UnixStream;
use tokio_util::codec::{Framed, LinesCodec};

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("could not connect to enwiro-daemon at {path:?}: {source}")]
    Connect {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("rpc transport error: {0}")]
    Transport(#[from] tokio_util::codec::LinesCodecError),
    #[error("rpc encode/decode error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("daemon dropped the connection before answering")]
    Disconnected,
    #[error("daemon returned response with mismatched id: expected {expected}, got {got}")]
    IdMismatch { expected: u64, got: u64 },
    #[error(transparent)]
    Rpc(#[from] RpcError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub struct Client {
    framed: Framed<UnixStream, LinesCodec>,
    next_id: AtomicU64,
}

impl Client {
    /// Connect to the daemon at the well-known path (`$ENWIRO_RPC_SOCKET`
    /// if set, otherwise the XDG-derived default).
    pub async fn connect() -> Result<Self, ClientError> {
        let path = if let Ok(p) = std::env::var(crate::rpc::SOCKET_ENV_VAR) {
            PathBuf::from(p)
        } else {
            default_socket_path()
                .map_err(|e| ClientError::Io(std::io::Error::other(e.to_string())))?
        };
        Self::connect_at(&path).await
    }

    /// Connect to the daemon at an explicit socket path. Useful for tests
    /// where the daemon binds inside a tempdir.
    pub async fn connect_at(path: &Path) -> Result<Self, ClientError> {
        let stream = UnixStream::connect(path)
            .await
            .map_err(|source| ClientError::Connect {
                path: path.to_path_buf(),
                source,
            })?;
        Ok(Self {
            framed: Framed::new(stream, LinesCodec::new_with_max_length(MAX_FRAME_BYTES)),
            next_id: AtomicU64::new(1),
        })
    }

    /// Send one request, await one response, deserialise into `R`.
    pub async fn call<R: serde::de::DeserializeOwned>(
        &mut self,
        req: Request,
    ) -> Result<R, ClientError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let envelope = RequestEnvelope::new(id, req);
        let serialised = serde_json::to_string(&envelope)?;
        self.framed.send(serialised).await?;
        let line = self
            .framed
            .next()
            .await
            .ok_or(ClientError::Disconnected)??;
        let resp: ResponseEnvelope = serde_json::from_str(&line)?;
        match resp.id {
            None => {
                // Server couldn't parse the request and has no id to echo
                // back. Body always carries the diagnostic error in this
                // case (see ResponseEnvelope::parse_error).
                if let ResponseBody::Err { error } = resp.body {
                    return Err(ClientError::Rpc(error));
                }
                return Err(ClientError::Rpc(RpcError {
                    code: RpcError::INTERNAL_ERROR,
                    message: "daemon returned response with no id and no error".into(),
                    data: None,
                }));
            }
            Some(got) if got != id => {
                return Err(ClientError::IdMismatch { expected: id, got });
            }
            Some(_) => {}
        }
        match resp.body {
            ResponseBody::Ok { result } => Ok(serde_json::from_value(result)?),
            ResponseBody::Err { error } => Err(ClientError::Rpc(error)),
        }
    }

    /// Typed wrapper for `cookbook.invoke`. Delegates a cookbook op through
    /// the daemon and returns the cookbook's stdout as a `CookbookInvokeResult`.
    pub async fn cookbook_invoke(
        &mut self,
        params: CookbookInvokeParams,
    ) -> Result<CookbookInvokeResult, ClientError> {
        self.call(Request::CookbookInvoke(params)).await
    }
}
