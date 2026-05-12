//! Machine-consumable endpoints for an external orchestrator
//! ([issue #3](https://github.com/coilysiren/repo-recall/issues/3)).
//!
//! - `GET /api/action-required` is a thin slice of the dashboard's
//!   action-required list — what would otherwise force the orchestrator
//!   to scrape the HTML or pull the whole dashboard JSON every tick.
//! - `POST /api/refresh` is the sync sibling of `POST /refresh`: it awaits
//!   the scan and returns the new `scan_version`, so a poller doesn't have
//!   to subscribe to the WebSocket to know "fresh data is now available."
//! - `GET /api/scan-version` is the cheapest possible "did anything change"
//!   check — `{ "scan_version": N }`. Pair with the `ETag` on the JSON
//!   endpoints if your client groks `If-None-Match`.

use std::sync::atomic::Ordering;

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

use crate::db::Span;
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
    /// (`rebase` / `merge` / etc.), or the failing CI text.
    pub detail: String,
    /// One entry per outstanding review-requested PR (only set when
    /// `signal == "review_requested"`). Each carries the PR number and the
    /// list of changed-file paths fetched alongside the count during the
    /// remote-state pass. Empty otherwise.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub review_requested_files: Vec<crate::db::ReviewRequestedPr>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ActionRequiredResponse {
    pub repos: Vec<ActionRequiredItem>,
    pub generated_at: i64,
    pub scan_version: u64,
}

/// `GET /api/action-required` — JSON-only. Always returns JSON regardless of
/// `Accept` (this endpoint exists *for* the JSON consumer; HTML browsers go
/// to `/`). Honors `If-None-Match` against the `scan_version` ETag so a
/// polling client gets `304` between scans.
pub async fn action_required(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let cache = state.cache_db.clone();
    let stale_after = stale_ask_threshold_secs();
    let (repos, dispatch_sigs) = tokio::task::spawn_blocking(move || {
        let repos = cache.list_repos_with_counts().unwrap_or_default();
        let sigs = cache.dispatch_signals(stale_after).unwrap_or_default();
        (repos, sigs)
    })
    .await
    .unwrap_or_default();

    let mut items = Vec::new();
    for r in &repos {
        for sig in derive_action_signals(r) {
            let review_requested_files = if sig.signal == "review_requested" {
                r.review_requested_pr_files.clone()
            } else {
                Vec::new()
            };
            items.push(ActionRequiredItem {
                id: format!("{}:{}", r.id, sig.signal),
                repo_id: r.id,
                repo_name: r.name.clone(),
                repo_path: r.path.clone(),
                remote_url: r.remote_url.clone(),
                signal: sig.signal,
                detail: sig.detail,
                review_requested_files,
            });
        }
    }
    // Append dispatch-substrate signals (#92, #115). Look up repo
    // identity per signal so the JSON envelope stays self-contained.
    let repo_lookup: std::collections::HashMap<i64, &crate::db::Repo> =
        repos.iter().map(|r| (r.id, r)).collect();
    for s in &dispatch_sigs {
        if let Some(r) = repo_lookup.get(&s.repo_id) {
            items.push(ActionRequiredItem {
                id: format!("{}:{}", s.repo_id, s.signal),
                repo_id: s.repo_id,
                repo_name: r.name.clone(),
                repo_path: r.path.clone(),
                remote_url: r.remote_url.clone(),
                signal: s.signal,
                detail: s.detail.clone(),
                review_requested_files: Vec::new(),
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

/// Threshold for the `stale_ask` signal, in seconds. Configurable via
/// `REPO_RECALL_STALE_ASK_DAYS` (default 7). Designed in #92, #115:
/// "the point of this system is to inspire work."
pub(crate) fn stale_ask_threshold_secs() -> i64 {
    std::env::var("REPO_RECALL_STALE_ASK_DAYS")
        .ok()
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(7)
        * 86_400
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
pub struct RefreshSyncResponse {
    pub scan_version: u64,
    pub last_scan: Option<i64>,
    /// True if a refresh actually ran. False if another refresh was in
    /// flight and this call coalesced — the returned `scan_version` will
    /// jump as soon as the in-flight refresh lands.
    pub ran: bool,
}

/// `POST /api/refresh` — runs a refresh inline and returns the new
/// `scan_version`. Sync sibling of `POST /refresh`, which returns 202 and
/// asks the caller to watch the WebSocket. If another refresh holds the
/// lock, this call coalesces (`ran=false`) rather than queueing — same
/// semantics as the HTML refresh button.
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

// Signal derivation lives in `crate::signals` so both the HTTP and MCP
// surfaces (`routes::api`, `mcp::tools`) call into one helper.

#[derive(Debug, Deserialize, Default)]
pub struct SpansQuery {
    /// Filter to spans whose `repo` attribute matches exactly. Optional.
    #[serde(default)]
    pub repo: Option<String>,
    /// Lower-bound on `start_time_unix_nano`, expressed in unix seconds for
    /// dictation friendliness. The handler converts to nanos before the
    /// table scan.
    #[serde(default)]
    pub since: Option<i64>,
    /// Cap on the result count. Defaults to 100, hard-capped at 1000 so a
    /// pathological producer cannot OOM the meta-loop.
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SpansResponse {
    pub spans: Vec<Span>,
    pub scan_version: u64,
}

/// `GET /api/spans` — query the LUCA substrate's inter-agent traffic
/// (luca#27). JSON-only. Filters by repo and time window, returns spans
/// newest-first. Designed for the meta-loop skill (coilyco-ai#224) to
/// poll on a cadence; honors `If-None-Match` against the scan_version
/// ETag so polls between refreshes return 304.
pub async fn spans(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<SpansQuery>,
) -> Response {
    let limit = q.limit.unwrap_or(100).min(1000);
    let since_nanos = q.since.map(|s| s.saturating_mul(1_000_000_000));
    let repo = q.repo.clone();
    let cache = state.cache_db.clone();
    let spans = match tokio::task::spawn_blocking(move || {
        cache.query_spans(repo.as_deref(), since_nanos, limit)
    })
    .await
    {
        Ok(Ok(v)) => v,
        _ => Vec::new(),
    };
    let body = SpansResponse {
        spans,
        scan_version: state.scan_version.load(Ordering::Acquire),
    };
    json_with_etag(&headers, body.scan_version, &body)
}

/// `GET /api/structural-asks` - every open `structural-ask` issue across
/// the workspace, newest-first. Used by the recall-dispatch preflight
/// (#92, #114) and the dashboard Autonomy panel.
pub async fn structural_asks(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let cache = state.cache_db.clone();
    let rows = match tokio::task::spawn_blocking(move || {
        cache.labeled_issues_by_state("structural-ask", "open")
    })
    .await
    {
        Ok(Ok(r)) => r,
        _ => Vec::new(),
    };
    let v = state.scan_version.load(Ordering::Acquire);
    json_with_etag(
        &headers,
        v,
        &serde_json::json!({
            "label": "structural-ask",
            "state": "open",
            "asks": rows,
            "scan_version": v,
        }),
    )
}

/// `GET /api/repos/{id}/dispatches` - parsed dispatch records from this
/// repo's `docs/repo-dispatch/` directory, newest-first (#92, #113).
/// JSON-only, ETag on `scan_version`.
pub async fn repo_dispatches(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(repo_id): Path<i64>,
) -> Response {
    let cache = state.cache_db.clone();
    let rows = match tokio::task::spawn_blocking(move || cache.dispatches_for_repo(repo_id)).await {
        Ok(Ok(r)) => r,
        _ => Vec::new(),
    };
    let v = state.scan_version.load(Ordering::Acquire);
    json_with_etag(
        &headers,
        v,
        &serde_json::json!({
            "repo_id": repo_id,
            "dispatches": rows,
            "scan_version": v,
        }),
    )
}

/// `GET /api/repos/{id}/tickets/{n}/history` - sessions + commits touching
/// issue `n` in repo `id`. Powers `recall_ticket_history` (#112) and the
/// per-repo dispatch view (#117). JSON-only, ETag on `scan_version`.
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

/// `GET /api/traces/{trace_id}` - all spans for one trace, chronological.
/// Reuses `SpansResponse`. Caller assembles the parent/child tree from
/// `parent_span_id`. Exists so consumers (LUCA meta-loop) can see agent
/// call shape - depth, fan-out, dead-end subagents - which the flat
/// `/api/spans` list hides. JSON-only, ETag on `scan_version`.
pub async fn trace(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(trace_id): Path<String>,
) -> Response {
    let cache = state.cache_db.clone();
    let spans =
        match tokio::task::spawn_blocking(move || cache.query_spans_by_trace(&trace_id)).await {
            Ok(Ok(v)) => v,
            _ => Vec::new(),
        };
    let body = SpansResponse {
        spans,
        scan_version: state.scan_version.load(Ordering::Acquire),
    };
    json_with_etag(&headers, body.scan_version, &body)
}
