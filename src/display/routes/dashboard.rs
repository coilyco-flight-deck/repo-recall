//! `/` - JSON dashboard projection.
//!
//! Returns the same data the old HTML dashboard used to render, as JSON.
//! Carries an `ETag` keyed on the monotonic scan version so a polling
//! consumer gets `304 Not Modified` between scans.

use std::sync::atomic::Ordering;

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::Response;
use serde::{Deserialize, Serialize};

use crate::display::routes::api::ActionRequiredItem;
use crate::display::routes::negotiate::json_with_etag;
use crate::process::activity;
use crate::signals::derive_action_signals;
use crate::{db, AppState};

#[derive(Debug, Deserialize, Default)]
pub struct DashboardParams {
    /// `me` -> filter to the detected git user's email. `all` -> no filter
    /// (the default). Any other string is treated as a literal email.
    #[serde(default)]
    pub author: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DashboardJson {
    pub repos: Vec<RepoJson>,
    pub recent_sessions: Vec<db::SessionWithRepos>,
    pub recent_commits: Vec<db::CommitWithRepo>,
    pub uncommitted_groups: Vec<db::UncommittedGroup>,
    pub ci_failures: Vec<db::CiFailure>,
    pub action_required: Vec<ActionRequiredItem>,
    pub banner: BannerCounts,
    pub counts: DashboardCounts,
    pub gh_health: &'static str,
    pub last_scan: Option<i64>,
    pub earliest_session: Option<i64>,
    pub author_filter: Option<String>,
    pub scan_version: u64,
    pub generated_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct RepoJson {
    #[serde(flatten)]
    pub repo: db::Repo,
    pub action_required: bool,
    pub action_signals: Vec<&'static str>,
    pub activity_score: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct BannerCounts {
    pub ci_failing: usize,
    pub dirty_repos: usize,
    pub in_progress_ops: usize,
    pub detached_heads: usize,
    pub review_requested: i64,
    pub issue_assigned: i64,
    pub deploy_failing: usize,
    pub deploy_stale: usize,
    pub stale_branches: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct DashboardCounts {
    pub repos: i64,
    pub sessions: i64,
    pub links: i64,
    pub commits: i64,
}

pub async fn index(
    State(state): State<AppState>,
    Query(params): Query<DashboardParams>,
    headers: HeaderMap,
) -> Response {
    let my_email = state.my_git_email.lock().await.clone();
    let author_filter: Option<String> = match params.author.as_deref() {
        None | Some("") | Some("all") => None,
        Some("me") => my_email.clone(),
        Some(email) => Some(email.to_string()),
    };
    let filter_label = author_filter.clone();

    let state2 = state.clone();
    let af = author_filter.clone();
    let data = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        let cache = &state2.cache_db;
        let (repos_n, sessions_n, links_n, commits_n) = cache.counts()?;
        let earliest_ts = cache.earliest_session_ts()?;
        let mut repos = cache.list_repos_with_counts()?;
        activity::sort(&mut repos);
        let recent_sessions = cache.recent_sessions(15)?;
        let recent_commits = cache.recent_commits(15, af.as_deref())?;
        let uncommitted_groups = cache.uncommitted_by_repo(6, 4)?;
        let ci_failures = cache.failing_ci_repos()?;
        Ok((
            repos_n,
            sessions_n,
            links_n,
            commits_n,
            earliest_ts,
            repos,
            recent_sessions,
            recent_commits,
            uncommitted_groups,
            ci_failures,
        ))
    })
    .await
    .unwrap();

    let (
        repos_n,
        sessions_n,
        links_n,
        commits_n,
        earliest_ts,
        repos,
        recent_sessions,
        recent_commits,
        uncommitted_groups,
        ci_failures,
    ) = match data {
        Ok(d) => d,
        Err(e) => {
            tracing::error!("dashboard query failed: {e:?}");
            let body = serde_json::json!({ "error": e.to_string() });
            return json_with_etag(&headers, 0, &body);
        }
    };

    let last_scan = *state.last_scan.lock().await;
    let viewer_str = viewer_state_str(&*state.viewer.lock().await);

    // Banner counters: skip vendored repos. Their signals are noise, not
    // action items.
    let ci_failing_count = repos
        .iter()
        .filter(|r| !activity::is_vendored(r) && r.ci_status.as_deref() == Some("failure"))
        .count();
    let dirty_count = repos
        .iter()
        .filter(|r| !activity::is_vendored(r) && (r.untracked_files + r.modified_files) > 0)
        .count();
    let in_progress_count = repos
        .iter()
        .filter(|r| !activity::is_vendored(r) && r.in_progress_op.is_some())
        .count();
    let detached_count = repos
        .iter()
        .filter(|r| !activity::is_vendored(r) && r.head_ref.as_deref() == Some("detached"))
        .count();
    let review_requested_count: i64 = repos.iter().map(|r| r.prs_awaiting_my_review).sum();
    let issue_assigned_count: i64 = repos.iter().map(|r| r.issues_assigned_to_me).sum();
    let deploy_failing_count = repos
        .iter()
        .filter(|r| activity::is_deploy_failing(r))
        .count();
    let deploy_stale_count = repos
        .iter()
        .filter(|r| activity::is_deploy_stale(r))
        .count();
    let stale_branches_count = repos
        .iter()
        .filter(|r| !activity::is_vendored(r) && activity::has_stale_branches(r))
        .count();

    let norms = activity::normalisers(&repos);
    let repo_json: Vec<RepoJson> = repos
        .iter()
        .map(|r| {
            let signals: Vec<&'static str> = derive_action_signals(r)
                .into_iter()
                .map(|s| s.signal)
                .collect();
            RepoJson {
                repo: r.clone(),
                action_required: activity::is_action_required(r),
                action_signals: signals,
                activity_score: activity::score(r, &norms),
            }
        })
        .collect();
    let mut action_items: Vec<ActionRequiredItem> = Vec::new();
    for r in &repos {
        for sig in derive_action_signals(r) {
            action_items.push(ActionRequiredItem {
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
    let body = DashboardJson {
        repos: repo_json,
        recent_sessions,
        recent_commits,
        uncommitted_groups,
        ci_failures,
        action_required: action_items,
        banner: BannerCounts {
            ci_failing: ci_failing_count,
            dirty_repos: dirty_count,
            in_progress_ops: in_progress_count,
            detached_heads: detached_count,
            review_requested: review_requested_count,
            issue_assigned: issue_assigned_count,
            deploy_failing: deploy_failing_count,
            deploy_stale: deploy_stale_count,
            stale_branches: stale_branches_count,
        },
        counts: DashboardCounts {
            repos: repos_n,
            sessions: sessions_n,
            links: links_n,
            commits: commits_n,
        },
        gh_health: viewer_str,
        last_scan: last_scan.map(|t| t.timestamp()),
        earliest_session: earliest_ts,
        author_filter: filter_label,
        scan_version: state.scan_version.load(Ordering::Acquire),
        generated_at: chrono::Utc::now().timestamp(),
    };
    json_with_etag(&headers, body.scan_version, &body)
}

fn viewer_state_str(
    v: &crate::ingest::github::RemoteFetchState<crate::ingest::github::AuthedUser>,
) -> &'static str {
    use crate::ingest::github::RemoteFetchState;
    match v {
        RemoteFetchState::Ok(_) => "ok",
        RemoteFetchState::Unconfigured => "unconfigured",
        RemoteFetchState::Unauthorized => "not_authenticated",
        RemoteFetchState::RateLimited { .. } => "rate_limited",
        RemoteFetchState::Missing => "missing",
        RemoteFetchState::Error(_) => "error",
    }
}
