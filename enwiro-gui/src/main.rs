//! enw-gui: a single binary that serves an embedded React SPA over localhost,
//! plus a typed `/api` (utoipa/OpenAPI) backed by the enwiro daemon + on-disk
//! environment metadata.
//!
//! The frontend lives in `web/` and is built by Vite into `web/dist`, which
//! `rust-embed` bakes into the binary at compile time.
//!
//! `enw-gui --dump-openapi` prints the OpenAPI document and exits — used by the
//! frontend codegen step (`@hey-api/openapi-ts`).

mod api;
mod board;

use std::sync::OnceLock;

use axum::Json;
use axum::extract::Request;
use axum::http::{StatusCode, Uri, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use enwiro_sdk::capability::CapabilitySet;
use rust_embed::RustEmbed;
use utoipa_axum::router::OpenApiRouter;

/// Shared handler state: where environments live on disk.
#[derive(Clone)]
pub struct AppState {
    pub workspaces_directory: String,
}

/// The compiled Vite build, embedded at compile time.
#[derive(RustEmbed)]
#[folder = "web/dist"]
struct Assets;

/// Serve an embedded asset, falling back to `index.html` for unknown paths so
/// client-side routes survive a reload (SPA fallback).
async fn static_handler(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };

    match Assets::get(path) {
        Some(file) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            ([(header::CONTENT_TYPE, mime.as_ref())], file.data).into_response()
        }
        None => match Assets::get("index.html") {
            Some(index) => ([(header::CONTENT_TYPE, "text/html")], index.data).into_response(),
            None => StatusCode::NOT_FOUND.into_response(),
        },
    }
}

/// Anti-DNS-rebinding: reject requests whose `Host` is not loopback. A rebound
/// DNS name resolving to 127.0.0.1 cannot forge this header, so a malicious page
/// can't drive our API. (Crib of the rmcp SDK's guard.)
async fn guard_host(req: Request, next: Next) -> Result<Response, StatusCode> {
    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    let hostname = host.split(':').next().unwrap_or("");
    if matches!(hostname, "127.0.0.1" | "localhost") {
        Ok(next.run(req).await)
    } else {
        Err(StatusCode::FORBIDDEN)
    }
}

/// This process's capability token (see `enwiro_sdk::capability`), minted once
/// at startup.
fn capability() -> &'static CapabilitySet {
    static CAPABILITY: OnceLock<CapabilitySet> = OnceLock::new();
    CAPABILITY.get_or_init(CapabilitySet::new)
}

/// Capability-token auth, Jupyter-style: loopback binding only stops *remote*
/// access, and `guard_host` above only stops a browser-driven DNS-rebinding
/// attack — neither stops another local process/user hitting this port
/// directly. Every `/api` request must carry the token minted for this
/// process, either as `?token=` (present on the `/` URL we print/open, in
/// case a caller wants to hit the API directly) or as `Authorization: Bearer`
/// (attached by the frontend to every call once it has read the token from
/// the URL).
///
/// Static assets (the compiled SPA shell) are deliberately NOT gated here:
/// the browser's requests for `/assets/*.js`/`.css` never carry the page
/// URL's query string, so gating them would 401 the frontend's own JS/CSS
/// before it ever runs and could read the token — the SPA shell isn't secret,
/// only the API surface it calls is.
async fn guard_capability(req: Request, next: Next) -> Result<Response, StatusCode> {
    if !req.uri().path().starts_with("/api") {
        return Ok(next.run(req).await);
    }
    let query_token = token_from_query(req.uri());
    let authorized = capability().is_authorized(req.headers())
        || query_token.is_some_and(|token| capability().contains(token));
    if authorized {
        Ok(next.run(req).await)
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}

fn token_from_query(uri: &Uri) -> Option<&str> {
    uri.query()?
        .split('&')
        .find_map(|pair| pair.strip_prefix("token="))
}

fn load_workspaces_directory() -> String {
    let config: enwiro_daemon::ConfigurationValues = enwiro_sdk::config::load_user_config("enwiro")
        .ok()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default();
    config.workspaces_directory
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let state = AppState {
        workspaces_directory: load_workspaces_directory(),
    };

    // Nest under `/api` at the OpenAPI level so the spec paths (`/api/board`,
    // …) match the served routes, then split into an axum Router + the doc.
    let (api_router, mut openapi) = OpenApiRouter::new()
        .nest("/api", api::router(state))
        .split_for_parts();
    openapi.info.title = "enw-gui".to_string();

    // `--dump-openapi`: emit the spec for frontend codegen, then exit.
    if std::env::args().any(|a| a == "--dump-openapi") {
        println!("{}", openapi.to_pretty_json()?);
        return Ok(());
    }

    let app = api_router
        .route(
            "/api/openapi.json",
            axum::routing::get(move || async move { Json(openapi) }),
        )
        .fallback(static_handler)
        .layer(axum::middleware::from_fn(guard_capability))
        .layer(axum::middleware::from_fn(guard_host));

    let token = capability().mint();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("http://{}/?token={token}", listener.local_addr()?);
    println!("enw-gui serving on {url}");

    let _ = open::that(&url);
    axum::serve(listener, app).await?;
    Ok(())
}
