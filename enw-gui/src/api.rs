//! The typed HTTP API the browser talks to. Handlers are annotated with
//! `#[utoipa::path]`; `utoipa-axum`'s `OpenApiRouter` auto-collects them into
//! an OpenAPI document that `@hey-api/openapi-ts` turns into the TS client.
//!
//! Reads (`/board`) come straight from disk; writes (`/env/mark`) are delegated
//! to the daemon over jsonrpsee — the same path the CLI kanban uses.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::AppState;
use crate::board::{Board, build_board};

#[derive(Deserialize, ToSchema)]
pub struct MarkRequest {
    pub env_name: String,
    /// One of: `ready`, `active`, `waiting`, `done`, `evergreen`.
    pub status: String,
}

#[derive(Serialize, ToSchema)]
pub struct MarkResponse {
    pub ok: bool,
}

/// Kanban board: environments grouped into status columns.
#[utoipa::path(
    get,
    path = "/board",
    responses((status = 200, body = Board)),
)]
async fn get_board(State(state): State<AppState>) -> Json<Board> {
    Json(build_board(&state.workspaces_directory).await)
}

/// Set an environment's status. Delegated to the daemon's `env.mark`.
#[utoipa::path(
    post,
    path = "/env/mark",
    request_body = MarkRequest,
    responses(
        (status = 200, body = MarkResponse),
        (status = 400, description = "daemon rejected the request (invalid status, unknown env)"),
        (status = 502, description = "daemon unreachable"),
    ),
)]
async fn post_mark(
    Json(req): Json<MarkRequest>,
) -> Result<Json<MarkResponse>, (StatusCode, String)> {
    use enwiro_sdk::rpc::{EnvMarkParams, EnwiroRpcClient, MarkSource};

    // Distinguish "couldn't reach the daemon at all" (502, a transport problem)
    // from "the daemon rejected this specific request" (400, the daemon was
    // reachable and had something concrete to say about why) — collapsing
    // both into the same status/empty body left callers unable to tell a bad
    // request apart from a dead daemon.
    let client = enwiro_sdk::rpc::connect().await.map_err(|e| {
        (
            StatusCode::BAD_GATEWAY,
            format!("could not reach enwiro daemon: {e}"),
        )
    })?;
    let result = client
        .env_mark(EnvMarkParams {
            env_name: req.env_name,
            status: req.status,
            source: MarkSource::User,
        })
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;

    Ok(Json(MarkResponse { ok: result.ok }))
}

/// Build the `/api` router and its OpenAPI document.
pub fn router(state: AppState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(get_board))
        .routes(routes!(post_mark))
        .with_state(state)
}
