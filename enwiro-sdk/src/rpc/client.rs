//! Client transport: jsonrpsee `async-client` driven over a unix domain
//! socket, framed by newline-delimited JSON via
//! `tokio_util::codec::LinesCodec`.
//!
//! No HTTP framing on the client side — `hyperlocal` is stale and HTTP
//! semantics don't earn their keep for a same-host same-user IPC. The
//! on-wire shape is one JSON-RPC envelope per line; `socat -u
//! UNIX-CONNECT:... - | jq .` consumes it natively.
//!
//! jsonrpsee 0.26 made `ClientBuilder::build_with_tokio(sender, receiver)`
//! the supported way to plug a custom duplex transport into the typed
//! client. Implementing `TransportSenderT` / `TransportReceiverT` over
//! the two halves of a split `UnixStream` is the entire transport layer.

use std::path::{Path, PathBuf};

use futures_util::{SinkExt, StreamExt};
use jsonrpsee::core::client::{
    Error as JsonRpcClientError, ReceivedMessage, TransportReceiverT, TransportSenderT,
};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio_util::codec::{FramedRead, FramedWrite, LinesCodec};

use crate::rpc::{SOCKET_ENV_VAR, default_socket_path};

/// Newline-delimited JSON frame limit (1 MiB). Caps memory against
/// pathological peer behaviour; today's payloads are well under 200 KiB.
const MAX_FRAME_BYTES: usize = 1024 * 1024;

pub type Client = jsonrpsee::core::client::Client;
pub type ClientError = JsonRpcClientError;

/// Connect to the daemon at the well-known path: `$ENWIRO_RPC_SOCKET`
/// if set (the daemon publishes it for its children), otherwise the
/// XDG-derived default.
pub async fn connect() -> anyhow::Result<Client> {
    let path = if let Ok(p) = std::env::var(SOCKET_ENV_VAR) {
        PathBuf::from(p)
    } else {
        default_socket_path()?
    };
    connect_at(&path).await
}

/// Connect to the daemon at an explicit socket path. Tests use this with
/// a tempdir-backed socket.
pub async fn connect_at(path: &Path) -> anyhow::Result<Client> {
    let stream = UnixStream::connect(path).await.map_err(|e| {
        anyhow::anyhow!(
            "could not connect to enwiro-daemon at {}: {}",
            path.display(),
            e
        )
    })?;
    let (read_half, write_half) = stream.into_split();

    let sender = UdsSender {
        writer: FramedWrite::new(write_half, LinesCodec::new_with_max_length(MAX_FRAME_BYTES)),
    };
    let receiver = UdsReceiver {
        reader: FramedRead::new(read_half, LinesCodec::new_with_max_length(MAX_FRAME_BYTES)),
    };

    Ok(jsonrpsee::core::client::ClientBuilder::default()
        .max_buffer_capacity_per_subscription(64)
        .build_with_tokio(sender, receiver))
}

/// Sender half of the custom transport. jsonrpsee hands us a `String`
/// per outbound JSON-RPC frame; we write it as one line.
struct UdsSender {
    writer: FramedWrite<OwnedWriteHalf, LinesCodec>,
}

impl TransportSenderT for UdsSender {
    type Error = TransportError;

    async fn send(&mut self, msg: String) -> Result<(), Self::Error> {
        self.writer
            .send(msg)
            .await
            .map_err(|e| TransportError(e.to_string()))
    }
}

/// Receiver half. Each inbound line is one JSON-RPC frame.
struct UdsReceiver {
    reader: FramedRead<OwnedReadHalf, LinesCodec>,
}

impl TransportReceiverT for UdsReceiver {
    type Error = TransportError;

    async fn receive(&mut self) -> Result<ReceivedMessage, Self::Error> {
        match self.reader.next().await {
            Some(Ok(line)) => Ok(ReceivedMessage::Text(line)),
            Some(Err(e)) => Err(TransportError(e.to_string())),
            None => Err(TransportError("daemon closed the connection".into())),
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("uds transport error: {0}")]
pub struct TransportError(String);
