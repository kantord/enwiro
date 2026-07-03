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
//! It binds to the container bridge gateway (not all interfaces), so the LAN
//! cannot reach it; if that address can't be determined or bound it fails closed
//! (does not start) rather than exposing the token more widely.
//!
//! v1 caveats: single global token; any *container* on the bridge can still reach
//! the proxy and spend inference quota (the credential itself is never exposed;
//! per-container auth is a follow-up); and it does not defend against server-side
//! tools (`web_search` runs on Anthropic infra and never traverses this proxy).

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
    let Some(listener) = bind_listener().await else {
        return;
    };
    let client = reqwest::Client::new();

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

/// Bind the proxy to the container bridge gateway (the address containers use to
/// reach the host, e.g. `172.17.0.1`) so that only bridge containers, not the
/// wider LAN, can reach it.
///
/// Fail-closed: if the gateway can't be determined or bound, the proxy does NOT
/// start. Binding to all interfaces instead would expose the auth token to every
/// process on the network, which defeats the point, so we refuse rather than
/// silently degrade. Claude launches then fail to authenticate (connection
/// refused), which is the safe failure.
async fn bind_listener() -> Option<TcpListener> {
    let Some(ip) = bridge_gateway_ip() else {
        tracing::error!(
            "claude auth proxy: could not determine the container bridge gateway; refusing to \
             start (binding all interfaces would expose the auth token). Claude launches will not \
             authenticate until this is resolved."
        );
        return None;
    };
    let addr = SocketAddr::new(ip, CLAUDE_PROXY_PORT);
    match TcpListener::bind(addr).await {
        Ok(listener) => {
            tracing::info!(%addr, "claude auth proxy listening (bridge gateway only)");
            Some(listener)
        }
        Err(error) => {
            tracing::error!(
                %error,
                %addr,
                "claude auth proxy: bridge-gateway bind failed; refusing to fall back to all \
                 interfaces (would expose the auth token)"
            );
            None
        }
    }
}

/// The container engine's default bridge gateway IP (what `host-gateway`, and
/// therefore the container, uses to reach the host). `None` if no engine, the
/// engine can't be queried, or the output doesn't parse. Uses Docker's inspect
/// format; other engines fall through to the all-interfaces bind.
fn bridge_gateway_ip() -> Option<std::net::IpAddr> {
    let engine = crate::launch::find_container_engine()?;
    let output = std::process::Command::new(engine)
        .args([
            "network",
            "inspect",
            "bridge",
            "--format",
            "{{range .IPAM.Config}}{{.Gateway}}{{end}}",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()?.trim().parse().ok()
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
