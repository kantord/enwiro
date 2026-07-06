//! Host-side Claude auth proxy (issue #540, experimental).
//!
//! A prompt-injected agent in the container could read a real OAuth token if we
//! injected one as an env var. This proxy keeps the real token on the *host*: the
//! container is pointed at
//! `ANTHROPIC_BASE_URL=http://host.containers.internal:<port>` with a per-launch
//! random capability token, and this proxy swaps that bearer for the real one
//! before forwarding to Anthropic. The credential never enters the container, so
//! it cannot be exfiltrated there.
//!
//! It is a transparent pass-through: only `Authorization` is rewritten, every
//! other header and the body are forwarded verbatim (the Claude Code harness
//! headers must survive to avoid tripping Anthropic's abuse detection), and the
//! response is streamed unbuffered so SSE works.
//!
//! It binds all interfaces (`0.0.0.0`): rootless Podman has no host-bindable
//! "bridge gateway" the way Docker does (verified — native bridge, forced
//! netavark bridge, and pasta's synthetic gateway are all either unbindable or
//! refuse loopback-only binds), so there is no narrower address to restrict to.
//! Access control lives entirely in the capability check below, not the bind
//! address.
//!
//! **Capability auth (this module's actual access control):** any *local*
//! process, or a hostile browser tab via DNS rebinding, can reach a bound TCP
//! port regardless of which address it's bound to — the same class of bug as
//! classic unauthenticated Jupyter servers. So every request must present a
//! bearer token this daemon itself minted for that specific launch; anything
//! else gets 401 before the real credential is ever used. Tokens are opaque,
//! random, held in memory only (no disk persistence, no cross-restart
//! carryover), and compared in constant time.
//!
//! v1 caveats: a capability is delivered to a container's `claude` shim and
//! remains valid for the daemon's lifetime (no per-container revocation yet, no
//! expiry); it does not defend against server-side tools (`web_search` runs on
//! Anthropic infra and never traverses this proxy); and it does not by itself
//! stop an *authorized* (capability-holding) but compromised claude process.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::OnceLock;

use bytes::Bytes;
use enwiro_sdk::capability::CapabilitySet;
use futures_util::StreamExt;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::body::{Frame, Incoming};
use hyper::header::HeaderMap;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;

/// Port the daemon's Claude auth proxy listens on. Shared with `launch.rs`,
/// which points the container's `ANTHROPIC_BASE_URL` at it.
pub(crate) const CLAUDE_PROXY_PORT: u16 = 8909;

const UPSTREAM: &str = "https://api.anthropic.com";

/// Capability tokens currently valid for use against this proxy, one per launch
/// that was given the claude shim. In-memory only: cleared on daemon restart:
/// there is no persistence and (yet) no per-container revocation, so a token
/// stays valid for the daemon's lifetime once minted. The set is expected to
/// stay tiny (proportional to concurrently-running enwiro-launched containers).
/// See `enwiro_sdk::capability` for the token format and comparison details.
fn capabilities() -> &'static CapabilitySet {
    static CAPABILITIES: OnceLock<CapabilitySet> = OnceLock::new();
    CAPABILITIES.get_or_init(CapabilitySet::new)
}

/// Mint a fresh random capability token, register it as valid, and return it for
/// the caller to hand to exactly one launch (via the claude shim).
pub(crate) fn mint_capability() -> String {
    capabilities().mint()
}

/// True iff `headers` carries `Authorization: Bearer <token>` for a token this
/// daemon minted.
fn is_authorized(headers: &HeaderMap) -> bool {
    capabilities().is_authorized(headers)
}

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

/// Bind the proxy on all interfaces. Rootless Podman has no host-bindable
/// "bridge gateway" to restrict to (see the module doc comment), so the
/// capability check in [`is_authorized`] is the real access control, not the
/// bind address. A bind failure (e.g. the port is already in use) is logged and
/// the proxy does not start; claude launches then fail to authenticate
/// (connection refused), which is the safe failure.
async fn bind_listener() -> Option<TcpListener> {
    let addr = SocketAddr::new(std::net::Ipv4Addr::UNSPECIFIED.into(), CLAUDE_PROXY_PORT);
    match TcpListener::bind(addr).await {
        Ok(listener) => {
            tracing::info!(%addr, "claude auth proxy listening");
            Some(listener)
        }
        Err(error) => {
            tracing::error!(%error, %addr, "claude auth proxy: bind failed; proxy will not start");
            None
        }
    }
}

/// Never fails at the hyper layer: an unauthorized request becomes a 401 and a
/// forwarding error becomes a 502, so the connection closes cleanly either way.
async fn handle(
    request: Request<Incoming>,
    client: reqwest::Client,
) -> Result<Response<ProxyBody>, Infallible> {
    if !is_authorized(request.headers()) {
        tracing::warn!("claude auth proxy: rejected request with no valid capability");
        return Ok(unauthorized_response());
    }
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

/// A bare 401 for a request that didn't present a valid capability. Per RFC
/// 6750, a bearer-auth failure carries `WWW-Authenticate: Bearer`.
fn unauthorized_response() -> Response<ProxyBody> {
    let body = BodyExt::boxed(Full::new(Bytes::new()).map_err(|never: Infallible| match never {}));
    Response::builder()
        .status(StatusCode::UNAUTHORIZED)
        .header(hyper::header::WWW_AUTHENTICATE, "Bearer")
        .body(body)
        .expect("static 401 response builds")
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

#[cfg(test)]
mod tests {
    // Token format, comparison, and header-parsing edge cases are covered by
    // `enwiro_sdk::capability`'s own test suite; these just confirm this
    // module's free-function wiring (shared process-wide `capabilities()`
    // singleton) delegates to it correctly.
    use super::*;

    fn bearer_headers(token: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            hyper::header::AUTHORIZATION,
            format!("Bearer {token}").parse().unwrap(),
        );
        headers
    }

    #[test]
    fn mint_produces_distinct_tokens_that_authorize() {
        let a = mint_capability();
        let b = mint_capability();
        assert_ne!(a, b);
        assert!(is_authorized(&bearer_headers(&a)));
        assert!(is_authorized(&bearer_headers(&b)));
    }

    #[test]
    fn an_unminted_token_does_not_authorize() {
        assert!(!is_authorized(&bearer_headers("not-a-real-capability")));
    }
}
