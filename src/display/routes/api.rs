//! Machine-consumable endpoints for an external orchestrator
//! ([issue #3](https://github.com/coilyco-flight-deck/repo-recall/issues/3)).

use std::sync::atomic::Ordering;

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

use crate::display::routes::negotiate::json_with_etag;
use crate::signals::derive_action_signals;
use crate::AppState;

#[derive(Debug, Clone, Serialize)]
pub struct ActionRequiredItem {
    /// Stable across scans for the same `(repo_id, signal)` combo. Lets
    /// the orchestrator tell "still broken" from "different problem now."
    pub id: String,
    pub repo_id: i64,
    pub repo_name: String,
    pub repo_path: String,
    pub remote_url: Option<String>,
    pub signal: &'static str,
    /// Short human-readable description of why this signal fired. Carries
    /// the count when relevant ("4 uncommitted files"), the op name
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ActionRequiredResponse {
    pub repos: Vec<ActionRequiredItem>,
    pub generated_at: i64,
    pub scan_version: u64,
}

/// `GET /api/action-required` — JSON-only. Always returns JSON regardless of
/// `Accept` (this endpoint exists *for* the JSON consumer; HTML browsers go
pub async fn action_required(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let cache = state.cache_db.clone();
    let repos =
        tokio::task::spawn_blocking(move || cache.list_repos_with_counts().unwrap_or_default())
            .await
            .unwrap_or_default();

    let mut items = Vec::new();
    for r in &repos {
        for sig in derive_action_signals(r) {
            items.push(ActionRequiredItem {
                id: format!("{}:{}", r.id, sig.signal),
                repo_id: r.id,
                repo_name: r.name.clone(),
                repo_path: r.path.clone(),
                remote_url: r.remote_url.clone(),
                signal: sig.signal,
                detail: sig.detail,
            });
        }
    }

    let body = ActionRequiredResponse {
        repos: items,
        generated_at: chrono::Utc::now().timestamp(),
        scan_version: state.scan_version.load(Ordering::Acquire),
    };
    json_with_etag(&headers, body.scan_version, &body)
}

#[derive(Debug, Clone, Serialize)]
pub struct ScanVersionResponse {
    pub scan_version: u64,
    /// Unix seconds of the last completed scan, or `null` if none yet.
    pub last_scan: Option<i64>,
}

/// `GET /api/scan-version` — single-integer poll target so a client doesn't
/// pay JSON-projection cost just to learn "did a refresh happen yet."
pub async fn scan_version(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let v = state.scan_version.load(Ordering::Acquire);
    let last = state.last_scan.lock().await.map(|t| t.timestamp());
    let body = ScanVersionResponse {
        scan_version: v,
        last_scan: last,
    };
    json_with_etag(&headers, v, &body)
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionsResponse {
    pub sessions: Vec<crate::db::SessionWithRepos>,
    pub generated_at: i64,
    pub scan_version: u64,
}

/// `GET /api/sessions` — every session in the cache as `Vec<SessionWithRepos>`,
/// unbounded by recency. ETag keyed on `scan_version`.
pub async fn sessions(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let cache = state.cache_db.clone();
    let sessions = tokio::task::spawn_blocking(move || cache.list_sessions().unwrap_or_default())
        .await
        .unwrap_or_default();
    let body = SessionsResponse {
        sessions,
        generated_at: chrono::Utc::now().timestamp(),
        scan_version: state.scan_version.load(Ordering::Acquire),
    };
    json_with_etag(&headers, body.scan_version, &body)
}

#[derive(Debug, Clone, Serialize)]
pub struct RefreshSyncResponse {
    pub scan_version: u64,
    pub last_scan: Option<i64>,
    /// True if a refresh actually ran. False if another refresh was in
    /// flight and this call coalesced — the returned `scan_version` will
    pub ran: bool,
}

/// `POST /api/refresh` — runs a refresh inline and returns the new
/// `scan_version`. Sync sibling of `POST /refresh`, which returns 202 and
pub async fn refresh_sync(State(state): State<AppState>) -> Response {
    let before = state.scan_version.load(Ordering::Acquire);
    let res = crate::display::routes::refresh::run_refresh(state.clone()).await;
    let after = state.scan_version.load(Ordering::Acquire);
    let last = state.last_scan.lock().await.map(|t| t.timestamp());
    let body = RefreshSyncResponse {
        scan_version: after,
        last_scan: last,
        ran: after > before,
    };
    if let Err(e) = res {
        tracing::error!("sync refresh failed: {e:?}");
    }
    axum::Json(body).into_response()
}

#[derive(Debug, Clone, Serialize)]
pub struct MilestoneView {
    #[serde(flatten)]
    pub milestone: crate::db::Milestone,
    pub repo_name: String,
    pub repo_path: String,
    pub repo_remote_url: Option<String>,
    /// Precomputed max of `updated_at`, `created_at`, `closed_at`.
    pub last_activity_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct MilestonesResponse {
    pub milestones: Vec<MilestoneView>,
    pub generated_at: i64,
    pub scan_version: u64,
}

/// `GET /api/milestones` - open milestones sorted by closest due date first.
/// Milestones with no `due_on` sort to the tail (#88).
pub async fn milestones(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let cache = state.cache_db.clone();
    let (milestones, repos) = tokio::task::spawn_blocking(move || {
        let ms = cache.list_open_milestones_by_due(0).unwrap_or_default();
        let rs = cache.list_repos_with_counts().unwrap_or_default();
        (ms, rs)
    })
    .await
    .unwrap_or_default();

    let mut by_id = std::collections::HashMap::with_capacity(repos.len());
    for r in &repos {
        by_id.insert(r.id, r);
    }

    let views: Vec<MilestoneView> = milestones
        .into_iter()
        .map(|m| {
            let r = by_id.get(&m.repo_id);
            let last_activity_at = m.last_activity_at();
            MilestoneView {
                repo_name: r.map(|r| r.name.clone()).unwrap_or_default(),
                repo_path: r.map(|r| r.path.clone()).unwrap_or_default(),
                repo_remote_url: r.and_then(|r| r.remote_url.clone()),
                last_activity_at,
                milestone: m,
            }
        })
        .collect();

    let body = MilestonesResponse {
        milestones: views,
        generated_at: chrono::Utc::now().timestamp(),
        scan_version: state.scan_version.load(Ordering::Acquire),
    };
    json_with_etag(&headers, body.scan_version, &body)
}

// Signal derivation lives in `crate::signals` so both the HTTP and MCP
// surfaces (`routes::api`, `mcp::tools`) call into one helper.

/// `GET /api/repos/{id}/tickets/{n}/history` - sessions + commits touching
/// issue `n` in repo `id`. Powers `recall_ticket_history` (#112) and the
pub async fn ticket_history(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((repo_id, issue_number)): Path<(i64, u32)>,
) -> Response {
    let cache = state.cache_db.clone();
    let history = match tokio::task::spawn_blocking(move || {
        cache.ticket_history(repo_id, issue_number)
    })
    .await
    {
        Ok(Ok(h)) => h,
        _ => crate::db::TicketHistory {
            repo_id,
            issue_number,
            sessions: Vec::new(),
            commits: Vec::new(),
        },
    };
    let v = state.scan_version.load(Ordering::Acquire);
    json_with_etag(&headers, v, &history)
}
