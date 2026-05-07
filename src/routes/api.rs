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

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

use crate::routes::negotiate::json_with_etag;
use crate::{activity, db, AppState};

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
    let state2 = state.clone();
    let repos =
        match tokio::task::spawn_blocking(move || state2.cache_db.list_repos_with_counts()).await {
            Ok(Ok(rs)) => rs,
            _ => Vec::new(),
        };

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
    let res = crate::routes::refresh::run_refresh(state.clone()).await;
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

pub struct DerivedSignal {
    pub signal: &'static str,
    pub detail: String,
}

/// Map a `Repo` row's individual fields onto the curated set of signals
/// that drive `is_action_required`. One repo can produce multiple items
/// (e.g. failing CI *and* a dirty tree) — the orchestrator can act on
/// each independently. Keep this list in sync with
/// [`activity::is_action_required`] — same triggers, just exploded into
/// per-signal records.
pub fn derive_action_signals(r: &db::Repo) -> Vec<DerivedSignal> {
    let mut out = Vec::new();
    if activity::is_vendored(r) {
        return out;
    }
    if r.ci_status.as_deref() == Some("failure") {
        out.push(DerivedSignal {
            signal: "ci_failing",
            detail: "default-branch CI failed".into(),
        });
    }
    let dirty = r.untracked_files + r.modified_files;
    if dirty > 0 {
        out.push(DerivedSignal {
            signal: "dirty_tree",
            detail: format!(
                "{dirty} uncommitted file{} ({} modified, {} untracked)",
                if dirty == 1 { "" } else { "s" },
                r.modified_files,
                r.untracked_files,
            ),
        });
    }
    if let Some(op) = r.in_progress_op.as_deref() {
        out.push(DerivedSignal {
            signal: "in_progress_op",
            detail: format!("{op} in progress"),
        });
    }
    if r.head_ref.as_deref() == Some("detached") {
        out.push(DerivedSignal {
            signal: "detached_head",
            detail: "HEAD is detached".into(),
        });
    }
    if r.prs_awaiting_my_review > 0 {
        let n = r.prs_awaiting_my_review;
        out.push(DerivedSignal {
            signal: "review_requested",
            detail: format!(
                "{n} PR{} awaiting your review",
                if n == 1 { "" } else { "s" },
            ),
        });
    }
    if r.my_draft_prs > 0 {
        let n = r.my_draft_prs;
        out.push(DerivedSignal {
            signal: "my_draft_pr",
            detail: format!(
                "{n} draft PR{} of yours - get into a reviewable state",
                if n == 1 { "" } else { "s" },
            ),
        });
    }
    if r.prs_mine_no_reviewer > 0 {
        let n = r.prs_mine_no_reviewer;
        out.push(DerivedSignal {
            signal: "pr_no_reviewer",
            detail: format!(
                "{n} of your open PR{} {} no reviewer - request one or self-merge",
                if n == 1 { "" } else { "s" },
                if n == 1 { "has" } else { "have" },
            ),
        });
    }
    if r.prs_mine_awaiting_review > 0 {
        let n = r.prs_mine_awaiting_review;
        out.push(DerivedSignal {
            signal: "my_open_pr",
            detail: format!(
                "{n} open PR{} of yours - test it before they review",
                if n == 1 { "" } else { "s" },
            ),
        });
    }
    if r.issues_assigned_to_me > 0 {
        let n = r.issues_assigned_to_me;
        out.push(DerivedSignal {
            signal: "issue_assigned",
            detail: format!("{n} issue{} assigned to you", if n == 1 { "" } else { "s" },),
        });
    }
    if activity::is_deploy_failing(r) {
        let wf = r.deploy_workflow.as_deref().unwrap_or("deploy");
        out.push(DerivedSignal {
            signal: "deploy_failing",
            detail: format!("last `{wf}` run on the default branch failed"),
        });
    } else if activity::is_deploy_stale(r) {
        let wf = r.deploy_workflow.as_deref().unwrap_or("deploy");
        let days = r
            .deploy_last_success_ts
            .map(|ts| (chrono::Utc::now().timestamp() - ts) / 86_400)
            .unwrap_or(0);
        out.push(DerivedSignal {
            signal: "deploy_stale",
            detail: format!("`{wf}` last green {days}d ago"),
        });
    }
    let _ = activity::is_action_required;
    out
}
