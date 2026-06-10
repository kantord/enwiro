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

use axum::Json;
use axum::extract::Request;
use axum::http::{StatusCode, Uri, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
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
        .layer(axum::middleware::from_fn(guard_host));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("http://{}", listener.local_addr()?);
    println!("enw-gui serving on {url}");

    let _ = open::that(&url);
    axum::serve(listener, app).await?;
    Ok(())
}
