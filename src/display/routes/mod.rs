use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;

use crate::display::mcp;
use crate::AppState;

pub mod api;
pub mod dashboard;
pub mod negotiate;
pub mod openapi;
pub mod refresh;

pub fn router(state: AppState) -> Router {
    let mcp_router = match mcp::http_router(state.clone()) {
        Ok(r) => Some(r),
        Err(e) => {
            tracing::error!("mcp http router build failed, /mcp disabled: {e:?}");
            None
        }
    };
    let base = Router::new()
        .route("/", get(dashboard::index))
        .route("/openapi.json", get(openapi::spec))
        .route("/api/action-required", get(api::action_required))
        .route(
            "/api/repos/{repo_id}/tickets/{issue_number}/history",
            get(api::ticket_history),
        )
        .route("/api/refresh", post(api::refresh_sync))
        .route("/api/scan-version", get(api::scan_version))
        .route("/api/sessions", get(api::sessions))
        .route("/api/milestones", get(api::milestones))
        .fallback(not_found_json)
        .with_state(state);
    match mcp_router {
        Some(r) => base.nest("/mcp", r),
        None => base,
    }
}

async fn not_found_json(uri: axum::http::Uri) -> Response {
    let body = serde_json::json!({
        "error": "not_found",
        "path": uri.to_string(),
    });
    let payload = serde_json::to_vec(&body).unwrap_or_else(|_| b"null".to_vec());
    let mut res = (StatusCode::NOT_FOUND, payload).into_response();
    res.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/json"),
    );
    res
}
