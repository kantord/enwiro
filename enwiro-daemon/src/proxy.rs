//! Host-side Claude auth proxy (issue #540, experimental).
//!
//! A prompt-injected agent in the container could read a real OAuth token if we
//! injected one as an env var. This proxy keeps the real token on the *host*: the
//! container is pointed at `ANTHROPIC_BASE_URL=http://host.docker.internal:<port>`
//! with a per-launch random capability token, and this proxy swaps that bearer
//! for the real one before forwarding to Anthropic. The credential never enters
//! the container, so it cannot be exfiltrated there.
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
//! **Capability auth (this module's access control):** the bridge-gateway bind
//! alone doesn't stop other *local* things from reaching the proxy — any process
//! on the bridge, or (bind-address-independent) any local process/hostile browser
//! tab via DNS rebinding, same class of bug as classic unauthenticated Jupyter
//! servers. So every request must present a bearer token this daemon itself
//! minted for that specific launch; anything else gets 401 before the real
//! credential is ever used. Tokens are opaque, random, held in memory only (no
//! disk persistence, no cross-restart carryover), and compared in constant time.
//!
//! v1 caveats: a capability is delivered to a container's `claude` shim and
//! remains valid for the daemon's lifetime (no per-container revocation yet, no
//! expiry); it does not defend against server-side tools (`web_search` runs on
//! Anthropic infra and never traverses this proxy); and it does not by itself
//! stop an *authorized* (capability-holding) but compromised claude process.

use std::collections::HashSet;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::{Mutex, OnceLock};

use bytes::Bytes;
use futures_util::StreamExt;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::body::{Frame, Incoming};
use hyper::header::HeaderMap;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use subtle::ConstantTimeEq;
use tokio::net::TcpListener;

/// Port the daemon's Claude auth proxy listens on. Shared with `launch.rs`,
/// which points the container's `ANTHROPIC_BASE_URL` at it.
pub(crate) const CLAUDE_PROXY_PORT: u16 = 8909;

const UPSTREAM: &str = "https://api.anthropic.com";

/// Length, in bytes, of a minted capability token before hex-encoding (32 bytes
/// = 256 bits, comfortably above the ~128-bit floor for a bearer secret).
const CAPABILITY_BYTES: usize = 32;

/// Capability tokens currently valid for use against this proxy, one per launch
/// that was given the claude shim. In-memory only: cleared on daemon restart:
/// there is no persistence and (yet) no per-container revocation, so a token
/// stays valid for the daemon's lifetime once minted. The set is expected to
/// stay tiny (proportional to concurrently-running enwiro-launched containers).
fn capabilities() -> &'static Mutex<HashSet<String>> {
    static CAPABILITIES: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    CAPABILITIES.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Mint a fresh random capability token, register it as valid, and return it for
/// the caller to hand to exactly one launch (via the claude shim). Never returns
/// a token that isn't also recorded as valid, so every minted token authorizes
/// immediately.
pub(crate) fn mint_capability() -> String {
    let mut bytes = [0u8; CAPABILITY_BYTES];
    rand::fill(&mut bytes);
    let token = hex_encode(&bytes);
    capabilities()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(token.clone());
    token
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// True iff `headers` carries `Authorization: Bearer <token>` for a token this
/// daemon minted. Compares in constant time (no early-exit on the first
/// differing byte, which would otherwise leak timing information about how much
/// of a guess matched) and checks every registered capability rather than
/// short-circuiting on the first candidate.
fn is_authorized(headers: &HeaderMap) -> bool {
    let Some(presented) = bearer_token(headers) else {
        return false;
    };
    let presented = presented.as_bytes();
    let tokens = capabilities()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut valid = subtle::Choice::from(0u8);
    for token in tokens.iter() {
        // `ct_eq` requires equal-length inputs; a length mismatch alone isn't
        // secret, so it's fine to check it (and skip) before the constant-time
        // byte comparison.
        if token.len() == presented.len() {
            valid |= token.as_bytes().ct_eq(presented);
        }
    }
    valid.into()
}

/// Extract the bearer value from an `Authorization: Bearer <token>` header, or
/// `None` if absent/malformed.
fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(hyper::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
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
    fn minted_capability_is_hex_and_correct_length() {
        let token = mint_capability();
        assert_eq!(token.len(), CAPABILITY_BYTES * 2, "{token}");
        assert!(token.chars().all(|c| c.is_ascii_hexdigit()), "{token}");
    }

    #[test]
    fn mint_produces_distinct_tokens() {
        assert_ne!(mint_capability(), mint_capability());
    }

    #[test]
    fn a_minted_capability_authorizes() {
        let token = mint_capability();
        assert!(is_authorized(&bearer_headers(&token)));
    }

    #[test]
    fn an_unminted_token_does_not_authorize() {
        // Exercises the shared, process-wide capability set (other tests in this
        // module mint real tokens into it), so use a value distinct from any real
        // 64-hex-char token: real capabilities are exactly `CAPABILITY_BYTES * 2`
        // hex characters, this deliberately isn't.
        assert!(!is_authorized(&bearer_headers("not-a-real-capability")));
    }

    #[test]
    fn missing_authorization_header_does_not_authorize() {
        assert!(!is_authorized(&HeaderMap::new()));
    }

    #[test]
    fn non_bearer_authorization_does_not_authorize() {
        let mut headers = HeaderMap::new();
        headers.insert(
            hyper::header::AUTHORIZATION,
            "Basic dXNlcjpwYXNz".parse().unwrap(),
        );
        assert!(!is_authorized(&headers));
    }
}
