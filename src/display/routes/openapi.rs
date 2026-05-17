//! Static OpenAPI 3.1 description of the JSON surface.
//!
//! Hand-maintained, not generated. The endpoint exists so a cold-start agent
//! that lands on any URL can follow the `Link: ...; rel="service-desc"`
//! header back to a machine-readable description of what's callable.

use axum::response::IntoResponse;
use axum::Json;
use serde_json::{json, Value};

pub async fn spec() -> impl IntoResponse {
    let doc: Value = json!({
        "openapi": "3.1.0",
        "info": {
            "title": "repo-recall",
            "version": "1",
            "description": "Local Claude Code session index. JSON-only surface; the MCP host runs in the same process at `/mcp`."
        },
        "paths": {
            "/": {
                "get": {
                    "summary": "Dashboard JSON view",
                    "description": "Returns the full dashboard projection: repos, recent sessions, recent commits, action-required signals, banner counts, autonomy rollup, structural asks. Carries an `ETag` keyed on the monotonic scan version; pass `If-None-Match` to short-circuit unchanged scans.",
                    "responses": {
                        "200": {"description": "Dashboard payload", "content": {"application/json": {}}},
                        "304": {"description": "Not Modified, ETag matched"}
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
                    "description": "Awaits the scan and returns the new scan_version.",
                    "responses": {"200": {"description": "Refresh complete", "content": {"application/json": {}}}}
                }
            },
            "/api/autonomy/metrics": {
                "get": {
                    "summary": "Autonomy rollup",
                    "description": "Per-repo autonomy / agent-readiness metrics.",
                    "responses": {"200": {"description": "Autonomy metrics", "content": {"application/json": {}}}}
                }
            },
            "/api/structural-asks": {
                "get": {
                    "summary": "Open structural-ask issues across the workspace",
                    "responses": {"200": {"description": "Structural-ask list", "content": {"application/json": {}}}}
                }
            },
            "/api/repos/{repo_id}/dispatches": {
                "get": {"summary": "List dispatches for a repo", "responses": {"200": {"description": "Dispatch list", "content": {"application/json": {}}}}},
                "post": {"summary": "Emit a dispatch artifact for a repo", "responses": {"200": {"description": "Dispatch result", "content": {"application/json": {}}}}}
            },
            "/api/repos/{repo_id}/tickets/{issue_number}/history": {
                "get": {
                    "summary": "Ticket history for a repo issue",
                    "responses": {"200": {"description": "Ticket history payload", "content": {"application/json": {}}}}
                }
            }
        }
    });
    Json(doc)
}
