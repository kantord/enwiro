//! enw-gui: a single binary that serves an embedded React SPA over
//! localhost and opens it in the user's browser.
//!
//! Hello-world scope: static asset serving only — no RPC, no daemon link
//! yet (see scratchpad / issue #626). The frontend lives in `web/` and is
//! built by Vite into `web/dist`, which `rust-embed` bakes into the binary
//! at compile time.

use axum::{
    Router,
    http::{StatusCode, Uri, header},
    response::{IntoResponse, Response},
};
use rust_embed::RustEmbed;

/// The compiled Vite build, embedded at compile time. In debug builds
/// rust-embed reads `web/dist` from disk on each request (rebuild the
/// frontend without recompiling Rust); in release builds the bytes are
/// baked into the binary.
#[derive(RustEmbed)]
#[folder = "web/dist"]
struct Assets;

/// Serve an embedded asset by path, falling back to `index.html` for any
/// unknown path so client-side routes survive a reload (SPA fallback).
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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let app = Router::new().fallback(static_handler);

    // Bind to a random free port on loopback only.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("http://{}", listener.local_addr()?);
    println!("enw-gui serving on {url}");

    let _ = open::that(&url);
    axum::serve(listener, app).await?;
    Ok(())
}
