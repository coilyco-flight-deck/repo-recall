// HTTP surface for the PWA push subscription lifecycle.
//
// - GET  /api/push/vapid-key   public key as text/plain b64url, fed to
//                              pushManager.subscribe on the frontend.
// - POST /api/push/subscribe   stores a PushSubscription.toJSON() body.
// - POST /api/push/unsubscribe removes one by endpoint URL.
//
// Dispatch lives in src/push.rs and runs from the refresh task; this
// module is just the subscription bookkeeping surface.

use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use crate::state::NewSubscription;
use crate::AppState;

/// Serve the service worker at the site root so its default scope is `/`,
/// not `/static/`. A SW served from `/static/sw.js` is constrained by the
/// browser to control only same-prefix URLs unless we also send a
/// Service-Worker-Allowed header — and the redirected scope is still
/// confusing. Easier to just serve the script at `/sw.js`.
pub async fn service_worker() -> Response {
    const SW_JS: &str = include_str!("../../static/sw.js");
    (
        [
            (
                header::CONTENT_TYPE,
                "application/javascript; charset=utf-8",
            ),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        SW_JS,
    )
        .into_response()
}

pub async fn vapid_key(State(state): State<AppState>) -> Response {
    match state.state_db.get_or_init_vapid() {
        Ok(v) => (
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            v.public_b64url,
        )
            .into_response(),
        Err(e) => {
            tracing::warn!("vapid init failed: {e:?}");
            (StatusCode::INTERNAL_SERVER_ERROR, "vapid unavailable").into_response()
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct SubscribeBody {
    pub endpoint: String,
    pub keys: SubscriptionKeysBody,
}

#[derive(Debug, Deserialize)]
pub struct SubscriptionKeysBody {
    pub p256dh: String,
    pub auth: String,
}

pub async fn subscribe(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<SubscribeBody>,
) -> Response {
    if state.demo_mode {
        return (StatusCode::FORBIDDEN, "disabled in demo mode\n").into_response();
    }
    let user_agent = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let new = NewSubscription {
        endpoint: body.endpoint,
        p256dh: body.keys.p256dh,
        auth: body.keys.auth,
        user_agent,
    };
    match state.state_db.upsert_subscription(&new) {
        Ok(()) => Json(json!({"ok": true})).into_response(),
        Err(e) => {
            tracing::warn!("upsert_subscription failed: {e:?}");
            (StatusCode::INTERNAL_SERVER_ERROR, "subscribe failed").into_response()
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct UnsubscribeBody {
    pub endpoint: String,
}

pub async fn unsubscribe(
    State(state): State<AppState>,
    Json(body): Json<UnsubscribeBody>,
) -> Response {
    if state.demo_mode {
        return (StatusCode::FORBIDDEN, "disabled in demo mode\n").into_response();
    }
    match state.state_db.remove_subscription(&body.endpoint) {
        Ok(_) => Json(json!({"ok": true})).into_response(),
        Err(e) => {
            tracing::warn!("remove_subscription failed: {e:?}");
            (StatusCode::INTERNAL_SERVER_ERROR, "unsubscribe failed").into_response()
        }
    }
}
