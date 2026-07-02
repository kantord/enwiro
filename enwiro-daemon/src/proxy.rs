//! Host-side Claude auth proxy (issue #540, experimental).
//!
//! A prompt-injected agent in the container could read a real OAuth token if we
//! injected one as an env var. This proxy keeps the real token on the *host*: the
//! container is pointed at `ANTHROPIC_BASE_URL=http://host.docker.internal:<port>`
//! with a throwaway sentinel `CLAUDE_CODE_OAUTH_TOKEN`, and this proxy swaps the
//! dummy `Authorization` header for the real one before forwarding to Anthropic.
//! The credential never enters the container, so it cannot be exfiltrated there.
//!
//! It is a transparent pass-through: only `Authorization` is rewritten, every
//! other header and the body are forwarded verbatim (the Claude Code harness
//! headers must survive to avoid tripping Anthropic's abuse detection), and the
//! response is streamed unbuffered so SSE works.
//!
//! v1 caveats: single global token, one shared listener on a fixed port bound to
//! all interfaces (any host/LAN process that can reach the port can use the
//! token via the proxy — acceptable for trusted local dev, tighten later), and
//! it does not defend against server-side tools (`web_search` runs on Anthropic
//! infra and never traverses this proxy).

use std::convert::Infallible;
use std::net::SocketAddr;

use bytes::Bytes;
use futures_util::StreamExt;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::body::{Frame, Incoming};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;

/// Port the daemon's Claude auth proxy listens on. Shared with `launch.rs`,
/// which points the container's `ANTHROPIC_BASE_URL` at it.
pub(crate) const CLAUDE_PROXY_PORT: u16 = 8909;

/// Sentinel `ANTHROPIC_AUTH_TOKEN` handed to the container so Claude Code enters
/// gateway mode and sends a request; the proxy replaces it with the real token.
/// Non-secret by design.
pub(crate) const CLAUDE_PROXY_SENTINEL_TOKEN: &str = "enwiro-proxy";

const UPSTREAM: &str = "https://api.anthropic.com";

type ProxyBody = BoxBody<Bytes, std::io::Error>;

/// Run the proxy until the process exits. Binding failure is logged and the task
/// returns (the daemon keeps running; a claude launch then just fails to auth
/// rather than taking the daemon down).
pub async fn serve() {
    let addr = SocketAddr::from(([0, 0, 0, 0], CLAUDE_PROXY_PORT));
    let listener = match TcpListener::bind(addr).await {
        Ok(listener) => listener,
        Err(error) => {
            tracing::error!(%error, port = CLAUDE_PROXY_PORT, "claude auth proxy: bind failed");
            return;
        }
    };
    let client = reqwest::Client::new();
    tracing::info!(port = CLAUDE_PROXY_PORT, "claude auth proxy listening");

    loop {
        let (stream, _peer) = match listener.accept().await {
            Ok(accepted) => accepted,
            Err(error) => {
                tracing::warn!(%error, "claude auth proxy: accept failed");
                continue;
            }
        };
        let client = client.clone();
        tokio::spawn(async move {
            let io = TokioIo::new(stream);
            let service = service_fn(move |request| handle(request, client.clone()));
            if let Err(error) = http1::Builder::new().serve_connection(io, service).await {
                tracing::debug!(%error, "claude auth proxy: connection ended");
            }
        });
    }
}

/// Never fails at the hyper layer: a forwarding error becomes a 502 so the
/// connection closes cleanly.
async fn handle(
    request: Request<Incoming>,
    client: reqwest::Client,
) -> Result<Response<ProxyBody>, Infallible> {
    match forward(request, client).await {
        Ok(response) => Ok(response),
        Err(error) => {
            tracing::warn!(%error, "claude auth proxy: forward failed");
            let body = BodyExt::boxed(
                Full::new(Bytes::from(format!("enwiro claude proxy error: {error}")))
                    .map_err(|never: Infallible| match never {}),
            );
            Ok(Response::builder()
                .status(StatusCode::BAD_GATEWAY)
                .body(body)
                .expect("static 502 response builds"))
        }
    }
}

/// Forward one request to Anthropic with the real `Authorization`, streaming the
/// response back. `Authorization` is the only header rewritten.
async fn forward(
    request: Request<Incoming>,
    client: reqwest::Client,
) -> anyhow::Result<Response<ProxyBody>> {
    let (parts, body) = request.into_parts();
    let path_and_query = parts
        .uri
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or("/");
    let url = format!("{UPSTREAM}{path_and_query}");
    let body_bytes = body.collect().await?.to_bytes();

    // hyper and reqwest share the `http` crate's `Method`/`HeaderName`, so these
    // pass straight through.
    let mut outbound = client.request(parts.method, &url).body(body_bytes);
    for (name, value) in parts.headers.iter() {
        // `host`/`content-length` are set by reqwest for the new request;
        // `authorization` is overridden below.
        if name == hyper::header::HOST
            || name == hyper::header::CONTENT_LENGTH
            || name == hyper::header::AUTHORIZATION
        {
            continue;
        }
        outbound = outbound.header(name, value);
    }
    if let Some(token) = crate::launch::claude_oauth_token() {
        outbound = outbound.header(hyper::header::AUTHORIZATION, format!("Bearer {token}"));
    }

    let upstream = outbound.send().await?;

    let mut builder = Response::builder().status(upstream.status());
    for (name, value) in upstream.headers().iter() {
        // We re-frame the body ourselves, so drop hop-by-hop / length headers
        // that no longer describe the re-streamed response.
        if name == hyper::header::CONTENT_LENGTH
            || name == hyper::header::TRANSFER_ENCODING
            || name == hyper::header::CONNECTION
        {
            continue;
        }
        builder = builder.header(name, value);
    }
    let stream = upstream
        .bytes_stream()
        .map(|chunk| chunk.map(Frame::data).map_err(std::io::Error::other));
    let body = BodyExt::boxed(StreamBody::new(stream));
    Ok(builder.body(body)?)
}
