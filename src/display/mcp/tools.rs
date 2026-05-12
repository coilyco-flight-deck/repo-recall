//! Tool handlers. Each handler takes a typed args struct, queries the data
//! layer via spawn_blocking (the redb readers are sync), and returns JSON.

use std::sync::atomic::Ordering;

use pmcp::RequestHandlerExtra;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::process::activity;
use crate::signals::derive_action_signals as derive_signals;
use crate::{db, display::routes, AppState};

// -----------------------------------------------------------------------------
// recall_dashboard
// -----------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DashboardArgs {}

#[derive(Debug, Serialize)]
struct DashboardRepo {
    id: i64,
    name: String,
    path: String,
    session_count: i64,
    commits_30d: i64,
    loc_churn_30d: i64,
    signals: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
struct ActionRequiredEntry {
    id: String,
    repo_id: i64,
    repo_name: String,
    repo_path: String,
    signal: &'static str,
    detail: String,
}

#[derive(Debug, Serialize)]
struct DashboardResult {
    scan_version: u64,
    last_scan: Option<i64>,
    session_count: i64,
    commits_30d: i64,
    repos: Vec<DashboardRepo>,
    action_required: Vec<ActionRequiredEntry>,
}

pub async fn dashboard(
    state: AppState,
    _args: DashboardArgs,
    _extra: RequestHandlerExtra,
) -> pmcp::Result<Value> {
    let cache = state.cache_db.clone();
    let mut repos = tokio::task::spawn_blocking(move || cache.list_repos_with_counts())
        .await
        .map_err(|e| pmcp::Error::internal(format!("join error: {e}")))?
        .map_err(|e| pmcp::Error::internal(format!("db error: {e}")))?;

    activity::sort(&mut repos);

    let session_count: i64 = repos.iter().map(|r| r.session_count).sum();
    let commits_30d: i64 = repos.iter().map(|r| r.commits_30d).sum();

    let mut action_required = Vec::new();
    for r in &repos {
        for sig in derive_signals(r) {
            action_required.push(ActionRequiredEntry {
                id: format!("{}:{}", r.id, sig.signal),
                repo_id: r.id,
                repo_name: r.name.clone(),
                repo_path: r.path.clone(),
                signal: sig.signal,
                detail: sig.detail,
            });
        }
    }

    let dashboard_repos: Vec<DashboardRepo> = repos
        .iter()
        .map(|r| {
            let signals: Vec<&'static str> =
                derive_signals(r).into_iter().map(|d| d.signal).collect();
            DashboardRepo {
                id: r.id,
                name: r.name.clone(),
                path: r.path.clone(),
                session_count: r.session_count,
                commits_30d: r.commits_30d,
                loc_churn_30d: r.loc_churn_30d,
                signals,
            }
        })
        .collect();

    let last_scan = state.last_scan.lock().await.map(|t| t.timestamp());
    let scan_version = state.scan_version.load(Ordering::Acquire);

    let result = DashboardResult {
        scan_version,
        last_scan,
        session_count,
        commits_30d,
        repos: dashboard_repos,
        action_required,
    };

    serde_json::to_value(result).map_err(|e| pmcp::Error::internal(format!("serialize: {e}")))
}

// -----------------------------------------------------------------------------
// recall_repo
// -----------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RepoArgs {
    /// Repo ID from `recall_dashboard`.
    pub repo_id: i64,
    /// Max commits to include. Default 50.
    #[serde(default)]
    pub commit_limit: Option<i64>,
}

pub async fn repo(
    state: AppState,
    args: RepoArgs,
    _extra: RequestHandlerExtra,
) -> pmcp::Result<Value> {
    let cache = state.cache_db.clone();
    let repo_id = args.repo_id;
    let commit_limit = args.commit_limit.unwrap_or(50);

    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<Value> {
        let repo = cache
            .get_repo(repo_id)?
            .ok_or_else(|| anyhow::anyhow!("repo {repo_id} not found"))?;
        let sessions = cache.sessions_for_repo(repo_id)?;
        let commits = cache.commits_for_repo(repo_id, commit_limit)?;
        let since_30d = chrono::Utc::now().timestamp() - 30 * 86_400;
        let hotspots = cache.file_hotspots(repo_id, since_30d, 10)?;

        Ok(json!({
            "repo": repo,
            "sessions": sessions,
            "commits": commits,
            "hotspots": hotspots,
        }))
    })
    .await
    .map_err(|e| pmcp::Error::internal(format!("join error: {e}")))?;

    result.map_err(|e| {
        let msg = e.to_string();
        if msg.contains("not found") {
            pmcp::Error::not_found(msg)
        } else {
            pmcp::Error::internal(msg)
        }
    })
}

// -----------------------------------------------------------------------------
// recall_session
// -----------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SessionArgs {
    /// Session row ID from `recall_dashboard` or `recall_search`.
    pub session_id: i64,
}

pub async fn session(
    state: AppState,
    args: SessionArgs,
    _extra: RequestHandlerExtra,
) -> pmcp::Result<Value> {
    let cache = state.cache_db.clone();
    let session_id = args.session_id;

    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<Value> {
        let session = cache
            .get_session(session_id)?
            .ok_or_else(|| anyhow::anyhow!("session {session_id} not found"))?;

        Ok(json!({
            "session": session.session,
            "repos": session.repos,
        }))
    })
    .await
    .map_err(|e| pmcp::Error::internal(format!("join error: {e}")))?;

    result.map_err(|e| {
        let msg = e.to_string();
        if msg.contains("not found") {
            pmcp::Error::not_found(msg)
        } else {
            pmcp::Error::internal(msg)
        }
    })
}

// -----------------------------------------------------------------------------
// recall_search
// -----------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchArgs {
    /// Free-text query. Searches repo names, session summaries, and commit subjects.
    pub q: String,
    /// Max hits per partition. Default 20.
    #[serde(default)]
    pub limit: Option<i64>,
}

pub async fn search(
    state: AppState,
    args: SearchArgs,
    _extra: RequestHandlerExtra,
) -> pmcp::Result<Value> {
    if args.q.trim().is_empty() {
        return Err(pmcp::Error::validation("query is empty"));
    }
    let limit = args.limit.unwrap_or(20);
    let hits = state
        .search_index
        .search(&args.q, limit as usize)
        .map_err(|e| pmcp::Error::internal(format!("search: {e}")))?;
    let hits: Vec<db::SearchHit> = hits
        .into_iter()
        .map(|h| db::SearchHit {
            kind: h.kind,
            ref_id: h.ref_id,
            text: h.text,
            extra: None,
        })
        .collect();

    serde_json::to_value(json!({
        "query": args.q,
        "hits": hits,
    }))
    .map_err(|e| pmcp::Error::internal(format!("serialize: {e}")))
}

// -----------------------------------------------------------------------------
// recall_action_required
// -----------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ActionRequiredArgs {}

pub async fn action_required(
    state: AppState,
    _args: ActionRequiredArgs,
    _extra: RequestHandlerExtra,
) -> pmcp::Result<Value> {
    let cache = state.cache_db.clone();
    let repos = tokio::task::spawn_blocking(move || cache.list_repos_with_counts())
        .await
        .map_err(|e| pmcp::Error::internal(format!("join error: {e}")))?
        .map_err(|e| pmcp::Error::internal(format!("db error: {e}")))?;

    let mut items = Vec::new();
    for r in &repos {
        for sig in derive_signals(r) {
            items.push(ActionRequiredEntry {
                id: format!("{}:{}", r.id, sig.signal),
                repo_id: r.id,
                repo_name: r.name.clone(),
                repo_path: r.path.clone(),
                signal: sig.signal,
                detail: sig.detail,
            });
        }
    }

    Ok(json!({
        "scan_version": state.scan_version.load(Ordering::Acquire),
        "generated_at": chrono::Utc::now().timestamp(),
        "repos": items,
    }))
}

// -----------------------------------------------------------------------------
// recall_ticket_history
// -----------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TicketHistoryArgs {
    /// Repo ID from `recall_dashboard`.
    pub repo_id: i64,
    /// GitHub issue (or PR) number.
    pub issue_number: u32,
}

/// Returns the sessions and commits in the cache that reference the named
/// issue in the named repo. Empty arrays when the issue is unindexed.
/// Designed in #92 to ground per-ticket recall-dispatch context in real
/// prior work.
pub async fn ticket_history(
    state: AppState,
    args: TicketHistoryArgs,
    _extra: RequestHandlerExtra,
) -> pmcp::Result<Value> {
    let cache = state.cache_db.clone();
    let history =
        tokio::task::spawn_blocking(move || cache.ticket_history(args.repo_id, args.issue_number))
            .await
            .map_err(|e| pmcp::Error::internal(format!("join error: {e}")))?
            .map_err(|e| pmcp::Error::internal(format!("db error: {e}")))?;

    serde_json::to_value(history).map_err(|e| pmcp::Error::internal(format!("serialize: {e}")))
}

// -----------------------------------------------------------------------------
// recall_refresh
// -----------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RefreshArgs {}

pub async fn refresh(
    state: AppState,
    _args: RefreshArgs,
    _extra: RequestHandlerExtra,
) -> pmcp::Result<Value> {
    let before = state.scan_version.load(Ordering::Acquire);
    routes::refresh::run_refresh(state.clone())
        .await
        .map_err(|e| pmcp::Error::internal(format!("refresh: {e}")))?;
    let after = state.scan_version.load(Ordering::Acquire);
    let last_scan = state.last_scan.lock().await.map(|t| t.timestamp());

    Ok(json!({
        "scan_version_before": before,
        "scan_version_after": after,
        "ran": after > before,
        "last_scan": last_scan,
    }))
}
