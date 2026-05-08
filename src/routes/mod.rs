use axum::http::{header, HeaderValue, Response};
use axum::middleware::{self, Next};
use axum::routing::{get, post};
use axum::Router;
use tower_http::services::ServeDir;

use crate::{mcp, AppState};

pub mod actions;
pub mod api;
pub mod dashboard;
pub mod fallback;
pub mod negotiate;
pub mod openapi;
pub mod push;
pub mod refresh;
pub mod repos;
pub mod search;
pub mod sessions;
pub mod templates;
pub mod ws;

pub fn router(state: AppState) -> Router {
    let static_dir = std::env::var("REPO_RECALL_STATIC")
        .ok()
        .unwrap_or_else(|| format!("{}/static", env!("CARGO_MANIFEST_DIR")));
    let mcp_router = match mcp::http_router(state.clone()) {
        Ok(r) => Some(r),
        Err(e) => {
            tracing::error!("mcp http router build failed, /mcp disabled: {e:?}");
            None
        }
    };
    let base = Router::new()
        .route("/", get(dashboard::index))
        .route("/repos/{id}", get(repos::detail))
        .route("/sessions/{id}", get(sessions::detail))
        .route("/search", get(search::search))
        .route("/refresh", post(refresh::trigger))
        .route("/openapi.json", get(openapi::spec))
        .route("/api/action-required", get(api::action_required))
        .route("/api/spans", get(api::spans))
        .route("/api/refresh", post(api::refresh_sync))
        .route("/api/scan-version", get(api::scan_version))
        .route("/api/repos/{id}/push", post(actions::push))
        .route("/api/repos/{id}/pull", post(actions::pull))
        .route("/api/clone", post(actions::clone_active))
        .route("/sw.js", get(push::service_worker))
        .route("/api/push/vapid-key", get(push::vapid_key))
        .route("/api/push/subscribe", post(push::subscribe))
        .route("/api/push/unsubscribe", post(push::unsubscribe))
        .route("/ws", get(ws::ws_handler))
        .route("/livereload", get(ws::livereload_handler))
        .nest_service("/static", ServeDir::new(static_dir))
        .fallback(fallback::not_found)
        .layer(middleware::from_fn(advertise_json_alternate))
        .with_state(state);
    match mcp_router {
        Some(r) => base.nest("/mcp", r),
        None => base,
    }
}

/// Advertise the JSON content-negotiation surface to discovering clients.
///
/// `Vary: Accept` is correctness: the same URL serves HTML or JSON depending
/// on the request, so any cache between us and the client must key on it.
/// The `Link` header points an agent at the dashboard's JSON variant without
/// requiring it to parse HTML for the `<link rel="alternate">` hint. Both
/// land on every response so a cold-start probe at any path finds the trail.
async fn advertise_json_alternate(
    req: axum::extract::Request,
    next: Next,
) -> Response<axum::body::Body> {
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    h.append(header::VARY, HeaderValue::from_static("Accept"));
    h.append(
        header::LINK,
        HeaderValue::from_static(
            "</?format=json>; rel=\"alternate\"; type=\"application/json\", \
             </openapi.json>; rel=\"service-desc\"; type=\"application/json\"",
        ),
    );
    res
}
