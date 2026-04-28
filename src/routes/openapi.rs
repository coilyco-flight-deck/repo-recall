//! Static OpenAPI 3.1 description of the JSON surface.
//!
//! Hand-maintained, not generated. The endpoint exists so a cold-start agent
//! that lands on any URL can follow the `Link: ...; rel="service-desc"`
//! header back to a machine-readable description of what's callable. Keep
//! the document narrow: dashboard JSON view + the `/api/*` orchestrator
//! slice. UI-only routes (`/repos/{id}`, `/sessions/{id}`, `/search`,
//! `/livereload`, `/ws`, push wiring) stay out — they're HTML or
//! WebSocket and not useful to an API consumer.

use axum::response::IntoResponse;
use axum::Json;
use serde_json::{json, Value};

pub async fn spec() -> impl IntoResponse {
    let doc: Value = json!({
        "openapi": "3.1.0",
        "info": {
            "title": "repo-recall",
            "version": "1",
            "description": "Local Claude Code session index. JSON surface served by content negotiation on `/` and dedicated endpoints under `/api/*`."
        },
        "paths": {
            "/": {
                "get": {
                    "summary": "Dashboard JSON view",
                    "description": "Returns the same data the HTML dashboard renders. Triggered by `Accept: application/json` or `?format=json`. Carries an `ETag` keyed on the monotonic scan version; pass `If-None-Match` to short-circuit unchanged scans.",
                    "parameters": [
                        {"name": "format", "in": "query", "schema": {"type": "string", "enum": ["json"]}, "required": false},
                        {"name": "Accept", "in": "header", "schema": {"type": "string"}, "required": false}
                    ],
                    "responses": {
                        "200": {"description": "Dashboard payload", "content": {"application/json": {}}},
                        "304": {"description": "Not Modified (ETag matched)"}
                    }
                }
            },
            "/api/action-required": {
                "get": {
                    "summary": "Repos with action_required signals",
                    "description": "Thin slice of the dashboard's action-required list. Stable across scans for the same `(repo_id, signal)` tuple via the `id` field.",
                    "responses": {"200": {"description": "List of action-required items", "content": {"application/json": {}}}}
                }
            },
            "/api/scan-version": {
                "get": {
                    "summary": "Cheapest 'did anything change' check",
                    "description": "Returns `{ \"scan_version\": N }` where N is the monotonic counter bumped at the end of every successful refresh.",
                    "responses": {"200": {"description": "Current scan version", "content": {"application/json": {}}}}
                }
            },
            "/api/refresh": {
                "post": {
                    "summary": "Synchronous refresh",
                    "description": "Awaits the scan and returns the new scan_version. Sync sibling of `POST /refresh` (which fires-and-forgets and uses HTMX/WebSocket for progress).",
                    "responses": {"200": {"description": "Refresh complete", "content": {"application/json": {}}}}
                }
            }
        }
    });
    Json(doc)
}
