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
    let stale_after = crate::display::routes::api::stale_ask_threshold_secs();
    let (repos, dispatch_sigs) = tokio::task::spawn_blocking(move || {
        (
            cache.list_repos_with_counts().unwrap_or_default(),
            cache.dispatch_signals(stale_after).unwrap_or_default(),
        )
    })
    .await
    .map_err(|e| pmcp::Error::internal(format!("join error: {e}")))?;

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
    let repo_lookup: std::collections::HashMap<i64, &db::Repo> =
        repos.iter().map(|r| (r.id, r)).collect();
    for s in &dispatch_sigs {
        if let Some(r) = repo_lookup.get(&s.repo_id) {
            items.push(ActionRequiredEntry {
                id: format!("{}:{}", s.repo_id, s.signal),
                repo_id: s.repo_id,
                repo_name: r.name.clone(),
                repo_path: r.path.clone(),
                signal: s.signal,
                detail: s.detail.clone(),
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
// recall_record_dispatch
// -----------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RecordDispatchArgs {
    /// Repo ID from `recall_dashboard`.
    pub repo_id: i64,
    /// `["owner/repo#N", ...]` cited tickets.
    pub issue_refs: Vec<String>,
    pub score: Option<i64>,
    pub autonomy_confidence: Option<i64>,
    pub autonomy_confidence_basis: Option<String>,
    /// `"owner/repo#M"` for the thin tracking issue, if one exists.
    pub tracking_issue: Option<String>,
    /// The verbatim prompt body.
    pub prompt: String,
    /// Optional override for the dispatch slug.
    pub slug: Option<String>,
}

/// Emit a new dispatch artifact (#92, #107). Writes a write-once
/// markdown file inside the repo at `docs/repo-dispatch/<slug>.md`
/// and mirrors it to `~/.repo-recall/dispatch/<repo>/<slug>.md` for
/// pollable consumption.
pub async fn record_dispatch(
    state: AppState,
    args: RecordDispatchArgs,
    _extra: RequestHandlerExtra,
) -> pmcp::Result<Value> {
    let cache = state.cache_db.clone();
    let repo_id = args.repo_id;
    let repo = tokio::task::spawn_blocking(move || cache.get_repo(repo_id))
        .await
        .map_err(|e| pmcp::Error::internal(format!("join error: {e}")))?
        .map_err(|e| pmcp::Error::internal(format!("db error: {e}")))?
        .ok_or_else(|| pmcp::Error::not_found(format!("repo {repo_id} not found")))?;
    let req = crate::display::dispatch_artifacts::EmitDispatchRequest {
        issue_refs: args.issue_refs,
        score: args.score,
        autonomy_confidence: args.autonomy_confidence,
        autonomy_confidence_basis: args.autonomy_confidence_basis,
        tracking_issue: args.tracking_issue,
        prompt: args.prompt,
        slug: args.slug,
    };
    let repo_path = std::path::PathBuf::from(&repo.path);
    let repo_name = repo.name.clone();
    let resp = tokio::task::spawn_blocking(move || {
        crate::display::dispatch_artifacts::emit_dispatch(&repo_path, &repo_name, &req)
    })
    .await
    .map_err(|e| pmcp::Error::internal(format!("join error: {e}")))?
    .map_err(|e| match e {
        crate::display::dispatch_artifacts::EmitError::AlreadyExists(p) => {
            pmcp::Error::validation(format!("dispatch already exists: {p}"))
        }
        crate::display::dispatch_artifacts::EmitError::NoIssueRefs
        | crate::display::dispatch_artifacts::EmitError::InvalidRef(_) => {
            pmcp::Error::validation(format!("invalid dispatch request: {e}"))
        }
        crate::display::dispatch_artifacts::EmitError::Io(_) => {
            pmcp::Error::internal(format!("emit io: {e}"))
        }
    })?;
    serde_json::to_value(resp).map_err(|e| pmcp::Error::internal(format!("serialize: {e}")))
}

// -----------------------------------------------------------------------------
// recall_autonomy_metrics
// -----------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AutonomyMetricsArgs {}

/// Aggregate AFK success rate from closed `repo-dispatch` tracking
/// issues, joined against `ISSUE_REFS` to detect commit-backed closes.
/// Returns overall + per-repo rates plus the bucketed counts. Empty
/// `per_repo` is the expected normal state until dispatches start
/// landing (#92, #116).
pub async fn autonomy_metrics(
    state: AppState,
    _args: AutonomyMetricsArgs,
    _extra: RequestHandlerExtra,
) -> pmcp::Result<Value> {
    let cache = state.cache_db.clone();
    let metrics = tokio::task::spawn_blocking(move || cache.autonomy_metrics())
        .await
        .map_err(|e| pmcp::Error::internal(format!("join error: {e}")))?
        .map_err(|e| pmcp::Error::internal(format!("db error: {e}")))?;
    serde_json::to_value(metrics).map_err(|e| pmcp::Error::internal(format!("serialize: {e}")))
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

// -----------------------------------------------------------------------------
// recall_open_structural_asks
// -----------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct OpenStructuralAsksArgs {}

/// List currently open `structural-ask`-labeled GitHub issues across the
/// indexed workspace (#92 phase 5, #105). The planner reads this before
/// drafting a new ask so it can refuse to re-ask a question already on
/// the list.
pub async fn open_structural_asks(
    state: AppState,
    _args: OpenStructuralAsksArgs,
    _extra: RequestHandlerExtra,
) -> pmcp::Result<Value> {
    let cache = state.cache_db.clone();
    let asks = tokio::task::spawn_blocking(move || {
        cache.labeled_issues_by_state("structural-ask", "open")
    })
    .await
    .map_err(|e| pmcp::Error::internal(format!("join error: {e}")))?
    .map_err(|e| pmcp::Error::internal(format!("db error: {e}")))?;
    let n = asks.len();
    serde_json::to_value(json!({ "count": n, "asks": asks }))
        .map_err(|e| pmcp::Error::internal(format!("serialize: {e}")))
}

// -----------------------------------------------------------------------------
// recall_emit_structural_ask
// -----------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct EmitStructuralAskArgs {
    pub title: String,
    pub ask_text: String,
    /// `["owner/repo#N", ...]` — tickets that would be unblocked.
    pub lifts: Vec<String>,
    pub slug: Option<String>,
}

/// Draft a new structural-context ask (#92 phase 5, #105). Writes a
/// write-once markdown file under `~/.repo-recall/structural-asks/`
/// for Kai to review and post as a `structural-ask`-labeled issue.
/// Free text is scrubbed via `process::sanitize` before write.
pub async fn emit_structural_ask(
    _state: AppState,
    args: EmitStructuralAskArgs,
    _extra: RequestHandlerExtra,
) -> pmcp::Result<Value> {
    use crate::process::structural_asks::{
        emit_structural_ask as emit, EmitError, EmitStructuralAskRequest,
    };
    let req = EmitStructuralAskRequest {
        title: args.title,
        ask_text: args.ask_text,
        lifts: args.lifts,
        slug: args.slug,
    };
    let resp = tokio::task::spawn_blocking(move || emit(&req))
        .await
        .map_err(|e| pmcp::Error::internal(format!("join error: {e}")))?
        .map_err(|e| match e {
            EmitError::AlreadyExists(p) => {
                pmcp::Error::validation(format!("structural ask already drafted: {p}"))
            }
            EmitError::EmptyTitle
            | EmitError::EmptyAskText
            | EmitError::NoLifts
            | EmitError::InvalidRef(_) => {
                pmcp::Error::validation(format!("invalid structural ask: {e}"))
            }
            EmitError::Io(_) => pmcp::Error::internal(format!("emit io: {e}")),
        })?;
    serde_json::to_value(resp).map_err(|e| pmcp::Error::internal(format!("serialize: {e}")))
}

// -----------------------------------------------------------------------------
// recall_emit_agents_drift_proposal
// -----------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct EmitAgentsDriftArgs {
    pub repo_slug: String,
    pub title: String,
    pub proposed_rule: String,
    /// `["owner/repo#N", ...]` — closed dispatches whose convergence
    /// motivates the rule.
    pub supporting_dispatches: Vec<String>,
    pub slug: Option<String>,
}

/// Draft an AGENTS.md drift proposal (#92 phase 5+, #106). Writes a
/// write-once markdown file under
/// `~/.repo-recall/agents-drift/<repo>/<slug>.md` for Kai to review
/// and post as a PR against `<repo>/AGENTS.md`. Free text is scrubbed
/// via `process::sanitize` before write.
pub async fn emit_agents_drift_proposal(
    _state: AppState,
    args: EmitAgentsDriftArgs,
    _extra: RequestHandlerExtra,
) -> pmcp::Result<Value> {
    use crate::process::agents_drift::{
        emit_drift_proposal as emit, EmitDriftProposalRequest, EmitError,
    };
    let req = EmitDriftProposalRequest {
        repo_slug: args.repo_slug,
        title: args.title,
        proposed_rule: args.proposed_rule,
        supporting_dispatches: args.supporting_dispatches,
        slug: args.slug,
    };
    let resp = tokio::task::spawn_blocking(move || emit(&req))
        .await
        .map_err(|e| pmcp::Error::internal(format!("join error: {e}")))?
        .map_err(|e| match e {
            EmitError::AlreadyExists(p) => {
                pmcp::Error::validation(format!("agents-drift proposal already drafted: {p}"))
            }
            EmitError::EmptyRepoSlug
            | EmitError::EmptyTitle
            | EmitError::EmptyRule
            | EmitError::NoSupportingDispatches
            | EmitError::InvalidRef(_) => {
                pmcp::Error::validation(format!("invalid agents-drift proposal: {e}"))
            }
            EmitError::Io(_) => pmcp::Error::internal(format!("emit io: {e}")),
        })?;
    serde_json::to_value(resp).map_err(|e| pmcp::Error::internal(format!("serialize: {e}")))
}
