use std::path::Path;
use std::sync::atomic::Ordering;

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use maud::{html, Markup};
use serde::{Deserialize, Serialize};

use crate::routes::api::ActionRequiredItem;
use crate::routes::negotiate::{json_with_etag, wants_json};
use crate::routes::templates::{
    absolute_time, compact_count, display_name, page, page_with_banners, relative_time,
    ACTION_PILL, H2, LI, LINK, META, PANEL, PANEL_ALERT, PATH, PILL, PILL_ALERT, PILL_FAINT, ROW,
    SCAN_STATUS,
};
use crate::signals::derive_action_signals;
use crate::{activity, db, AppState};

#[derive(Debug, Deserialize, Default)]
pub struct DashboardParams {
    /// `me` → filter to the detected git user's email. `all` → no filter
    /// (the default). Any other string is treated as a literal email.
    #[serde(default)]
    pub author: Option<String>,
    /// `json` switches the response to a JSON projection of the same data.
    /// Equivalent to `Accept: application/json`. See [issue #2].
    ///
    /// [issue #2]: https://github.com/coilysiren/repo-recall/issues/2
    #[serde(default)]
    pub format: Option<String>,
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
    // Resolve `?author=` into a concrete email filter. `me` needs to see the
    // cached git email; anything non-`all` / non-empty is used literally.
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
        // Capped at 6 repos × 4 files/repo = max 24 rows in the panel
        // (+headers), enough to read at a glance without scrolling forever.
        let uncommitted_groups = cache.uncommitted_by_repo(6, 4)?;
        let ci_failures = cache.failing_ci_repos()?;
        let uncloned = cache.uncloned_active_repos(25)?;
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
            uncloned,
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
        uncloned,
    ) = match data {
        Ok(d) => d,
        Err(e) => {
            tracing::error!("dashboard query failed: {e:?}");
            return page("error", html! { p { "Error: " (e.to_string()) } }).into_response();
        }
    };

    let last_scan = *state.last_scan.lock().await;
    let last_scan_str = last_scan
        .map(|t| absolute_time(Some(t.timestamp())))
        .unwrap_or_else(|| "never".into());
    let gh_health = *state.gh_health.lock().await;

    // Aggregate banner counts and per-repo signal sets up front so both the
    // JSON branch and the HTML branch read from the same numbers.
    // Banner counters: skip vendored repos. They're explicitly marked as
    // not-mine so their CI failures, dirty trees, in-progress git ops, and
    // detached HEADs are noise, not action items.
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
    let has_action = ci_failing_count
        + dirty_count
        + in_progress_count
        + detached_count
        + (review_requested_count as usize)
        + (issue_assigned_count as usize)
        + deploy_failing_count
        + deploy_stale_count
        > 0;

    if wants_json(&headers, params.format.as_deref()) {
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
                let review_requested_files = if sig.signal == "review_requested" {
                    r.review_requested_pr_files.clone()
                } else {
                    Vec::new()
                };
                action_items.push(ActionRequiredItem {
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
            },
            counts: DashboardCounts {
                repos: repos_n,
                sessions: sessions_n,
                links: links_n,
                commits: commits_n,
            },
            gh_health: gh_health_str(gh_health),
            last_scan: last_scan.map(|t| t.timestamp()),
            earliest_session: earliest_ts,
            author_filter: filter_label.clone(),
            scan_version: state.scan_version.load(Ordering::Acquire),
            generated_at: chrono::Utc::now().timestamp(),
        };
        return json_with_etag(&headers, body.scan_version, &body);
    }

    // Format "back to" line: "2025-11-12 (164d)" or "—" if we have no
    // sessions with timestamps yet (first boot before the initial scan lands).
    let earliest_str = earliest_ts
        .and_then(|ts| chrono::DateTime::<chrono::Utc>::from_timestamp(ts, 0))
        .map(|dt| {
            let days = (chrono::Utc::now() - dt).num_days().max(0);
            format!("{} ({}d)", dt.format("%Y-%m-%d"), days)
        })
        .unwrap_or_else(|| "—".into());

    // Group per-repo action signals by signal type so the expanded panel can
    // list "every dirty repo" together, "every failing CI" together, etc.
    // Order is deliberate: the noisier remote-state signals go first so the
    // user resolves the things that block teammates before sweeping local
    // dirt.
    type ActionGroup<'a> = (&'static str, &'static str, Vec<(&'a db::Repo, String)>);
    let action_groups: Vec<ActionGroup> = {
        use std::collections::BTreeMap;
        let mut by_signal: BTreeMap<&'static str, Vec<(&db::Repo, String)>> = BTreeMap::new();
        for r in &repos {
            for sig in derive_action_signals(r) {
                by_signal
                    .entry(sig.signal)
                    .or_default()
                    .push((r, sig.detail));
            }
        }
        let order: &[(&'static str, &'static str)] = &[
            ("ci_failing", "failing CI"),
            ("deploy_failing", "failing deploy"),
            ("deploy_stale", "stale deploy"),
            ("review_requested", "awaiting your review"),
            ("issue_assigned", "issues assigned to you"),
            ("in_progress_op", "mid-op"),
            ("detached_head", "detached HEAD"),
            ("dirty_tree", "dirty tree"),
        ];
        order
            .iter()
            .filter_map(|(k, label)| by_signal.remove(k).map(|v| (*k, *label, v)))
            .collect()
    };

    let body = html! {
        @if has_action {
            details class="mb-4 rounded-md bg-[#574f7d] text-white overflow-hidden shadow-sm" {
                summary class="cursor-pointer select-none px-3 py-2 text-xs
                               flex items-baseline gap-x-3 gap-y-1 flex-wrap
                               hover:bg-[#3e375d] transition-colors" {
                    span class="text-base leading-none" { "⚠" }
                    span class="font-bold uppercase tracking-[0.08em]" { "action required" }
                    @if ci_failing_count > 0 {
                        a class=(ACTION_PILL) data-action-pill href="#action-ci_failing" {
                            (ci_failing_count) " failing CI" @if ci_failing_count != 1 { "s" }
                        }
                    }
                    @if dirty_count > 0 {
                        a class=(ACTION_PILL) data-action-pill href="#action-dirty_tree" {
                            (dirty_count) " dirty " @if dirty_count == 1 { "repo" } @else { "repos" }
                        }
                    }
                    @if in_progress_count > 0 {
                        a class=(ACTION_PILL) data-action-pill href="#action-in_progress_op" {
                            (in_progress_count) " mid-op"
                        }
                    }
                    @if detached_count > 0 {
                        a class=(ACTION_PILL) data-action-pill href="#action-detached_head" {
                            (detached_count) " detached HEAD" @if detached_count != 1 { "s" }
                        }
                    }
                    @if review_requested_count > 0 {
                        a class=(ACTION_PILL) data-action-pill href="#action-review_requested" {
                            (review_requested_count) " awaiting your review"
                        }
                    }
                    @if issue_assigned_count > 0 {
                        a class=(ACTION_PILL) data-action-pill href="#action-issue_assigned" {
                            (issue_assigned_count) " issue" @if issue_assigned_count != 1 { "s" }
                            " assigned to you"
                        }
                    }
                    @if deploy_failing_count > 0 {
                        a class=(ACTION_PILL) data-action-pill href="#action-deploy_failing" {
                            (deploy_failing_count) " failing deploy"
                            @if deploy_failing_count != 1 { "s" }
                        }
                    }
                    @if deploy_stale_count > 0 {
                        a class=(ACTION_PILL) data-action-pill href="#action-deploy_stale" {
                            (deploy_stale_count) " stale deploy"
                            @if deploy_stale_count != 1 { "s" }
                        }
                    }
                }
                div class="px-3 pb-3 pt-1 text-sm leading-relaxed flex flex-col gap-3
                           border-t border-white/15" {
                    @for (signal, label, items) in &action_groups {
                        section id={ "action-" (signal) } class="scroll-mt-4" {
                            div class="font-bold uppercase tracking-[0.08em] mb-1
                                       text-[11px] text-white/80" {
                                (label) " (" (items.len()) ")"
                            }
                            ul class="list-none p-0 m-0 flex flex-col gap-1.5" {
                                @for (r, detail) in items {
                                    li { (action_sentence(signal, r, detail)) }
                                }
                            }
                        }
                    }
                }
                script src="/static/action-required.js" defer {}
            }
        }

        section class="flex gap-8 items-end mb-2 flex-wrap" {
            (stat("repos", &repos_n.to_string()))
            (stat("sessions", &sessions_n.to_string()))
            (stat("commits", &commits_n.to_string()))
            (stat("links", &links_n.to_string()))
            div {
                div class="text-[11px] uppercase tracking-[0.08em] text-[#9e9fc2] font-bold" { "earliest" }
                div class="text-sm text-[#574f7d] mt-1 font-mono" { (earliest_str) }
            }
            div {
                div class="text-[11px] uppercase tracking-[0.08em] text-[#9e9fc2] font-bold" { "last scan" }
                div class="text-sm text-[#574f7d] mt-1 font-mono" { (last_scan_str) }
            }
            (next_refresh_countdown(state.refresh_interval_secs, last_scan))
            (author_toggle(my_email.as_deref(), filter_label.as_deref()))
            form method="post" action="/refresh" hx-post="/refresh" hx-swap="none" {
                button type="submit"
                    class="bg-[#574f7d] text-white px-4 py-2 rounded-md text-xs font-bold tracking-wide
                           hover:bg-[#3e375d] hover:-translate-y-px hover:shadow-md
                           transition-all duration-150 cursor-pointer
                           shadow-sm" {
                    "↻ refresh"
                }
            }
        }

        p class="text-[11px] text-[#574f7d]/60 italic mb-3 max-w-3xl" {
            "History goes as far back as Claude Code has kept sessions on disk — we read every "
            code class="font-mono not-italic bg-[#9e9fc2]/15 px-1 rounded" { ".jsonl" }
            " under "
            code class="font-mono not-italic bg-[#9e9fc2]/15 px-1 rounded" { "~/.claude/projects/" }
            " and don't cap the range ourselves. If yours stops earlier than expected, Claude Code has rotated or cleaned them up."
        }

        (standup_details(&repos, &recent_commits, &recent_sessions, filter_label.as_deref()))

        div id="scan-status" hx-ext="ws" ws-connect="/ws" class=(SCAN_STATUS) {
            "waiting for scan status…"
        }
        // dashboard-reload.js opens its own /ws subscription to catch the
        // refresh-complete sentinel and call location.reload(). Scoped to
        // the dashboard so detail pages don't reload mid-read.
        script src="/static/dashboard-reload.js" defer {}

        @let uncommitted_total: i64 = uncommitted_groups.iter().map(|g| g.total).sum();
        @let uncommitted_panel = if uncommitted_groups.is_empty() { PANEL } else { PANEL_ALERT };
        @if !ci_failures.is_empty() {
            section class={ (PANEL_ALERT) " mb-4 border-l-[6px] bg-[#efe8f5]" } {
                h2 class="text-sm text-[#3e375d] font-bold uppercase tracking-[0.08em] mb-3
                         flex items-baseline gap-2" {
                    span class="text-base leading-none" { "✖" }
                    "CI failing — action required"
                    span class="text-[#574f7d]/70 normal-case tracking-normal font-normal text-xs" {
                        "(" (ci_failures.len()) " repo"
                        @if ci_failures.len() != 1 { "s" }
                        ")"
                    }
                }
                (render_ci_failures(&ci_failures))
            }
        }
        div class="grid grid-cols-1 lg:grid-cols-2 gap-4" {
            div class="flex flex-col gap-4 min-w-0" {
                section class=(PANEL) {
                    h2 class=(H2) { "repos" }
                    (render_repos(&repos, &state.cwd))
                }
                @if !uncloned.is_empty() {
                    section class=(PANEL) {
                        h2 class=(H2) {
                            "active on github, not cloned"
                            span class="text-[#574f7d]/70 normal-case tracking-normal font-normal" {
                                " (" (uncloned.len()) ")"
                            }
                        }
                        (render_uncloned(&uncloned))
                    }
                }
                @let needs_push: Vec<&db::Repo> = repos.iter()
                    .filter(|r| r.commits_ahead > 0).collect();
                @if !needs_push.is_empty() {
                    section class=(PANEL) {
                        h2 class=(H2) {
                            "needs push"
                            span class="text-[#574f7d]/70 normal-case tracking-normal font-normal" {
                                " (" (needs_push.len()) ")"
                            }
                        }
                        (render_needs_action(&needs_push, "push"))
                    }
                }
                @let needs_pull: Vec<&db::Repo> = repos.iter()
                    .filter(|r| r.commits_behind > 0).collect();
                @if !needs_pull.is_empty() {
                    section class=(PANEL) {
                        h2 class=(H2) {
                            "needs pull"
                            span class="text-[#574f7d]/70 normal-case tracking-normal font-normal" {
                                " (" (needs_pull.len()) ")"
                            }
                        }
                        (render_needs_action(&needs_pull, "pull"))
                    }
                }
            }
            div class="flex flex-col gap-4 min-w-0" {
                section class=(uncommitted_panel) {
                    h2 class={
                        (H2)
                        @if !uncommitted_groups.is_empty() { " text-[#3e375d]" }
                    } {
                        "uncommitted work"
                        @if !uncommitted_groups.is_empty() {
                            " — action required"
                            span class="text-[#574f7d]/70 normal-case tracking-normal font-normal" {
                                " ("
                                (compact_count(uncommitted_total)) " file"
                                @if uncommitted_total != 1 { "s" }
                                " across " (uncommitted_groups.len()) " repo"
                                @if uncommitted_groups.len() != 1 { "s" }
                                ")"
                            }
                        }
                    }
                    (render_uncommitted_groups(&uncommitted_groups))
                }
                section class=(PANEL) {
                    h2 class=(H2) { "recent sessions" }
                    (render_sessions(&recent_sessions))
                }
                section class=(PANEL) {
                    h2 class=(H2) { "recent commits" }
                    (render_commits(&recent_commits, &state.cwd))
                }
            }
        }
    };
    page_with_banners("dashboard", body, Some(gh_health)).into_response()
}

fn gh_health_str(h: crate::commits::GhHealth) -> &'static str {
    use crate::commits::GhHealth;
    match h {
        GhHealth::Ok => "ok",
        GhHealth::NotAuthenticated => "not_authenticated",
        GhHealth::Missing => "missing",
    }
}

/// Expandable standup summary — collapsed by default so it doesn't crowd the
/// main dashboard, but when you click it opens to a tight digest of the last
/// 24h: commits per repo, sessions today, action-required counts rolled up.
/// Intentionally lives on the main page (user spec: "keep people on the main
/// page 95% of the time").
fn standup_details(
    repos: &[db::Repo],
    recent_commits: &[db::CommitWithRepo],
    recent_sessions: &[db::SessionWithRepos],
    author_filter: Option<&str>,
) -> Markup {
    let now = chrono::Utc::now().timestamp();
    let cutoff = now - 86_400; // 24h

    // Commits in the last 24h, grouped by repo.
    use std::collections::BTreeMap;
    let mut by_repo: BTreeMap<String, Vec<&db::CommitWithRepo>> = BTreeMap::new();
    for c in recent_commits
        .iter()
        .filter(|c| c.commit.timestamp >= cutoff)
    {
        by_repo.entry(c.repo_name.clone()).or_default().push(c);
    }

    let sessions_today: Vec<_> = recent_sessions
        .iter()
        .filter(|sr| sr.session.started_at.map(|t| t >= cutoff).unwrap_or(false))
        .collect();

    let dirty: Vec<_> = repos
        .iter()
        .filter(|r| (r.untracked_files + r.modified_files) > 0)
        .collect();
    let failing_ci: Vec<_> = repos
        .iter()
        .filter(|r| r.ci_status.as_deref() == Some("failure"))
        .collect();

    html! {
        details class="mb-4 rounded-md border border-[#9e9fc2]/45 bg-[#f5f3f9] overflow-hidden" {
            summary class="cursor-pointer px-4 py-2 text-xs font-bold uppercase
                           tracking-[0.08em] text-[#574f7d] hover:bg-[#9e9fc2]/15 select-none
                           flex items-center gap-2" {
                span { "📝 today's standup" }
                span class="normal-case tracking-normal font-normal text-[#574f7d]/70" {
                    "("
                    (by_repo.len()) " repo"
                    @if by_repo.len() != 1 { "s" }
                    " committed to · "
                    (sessions_today.len()) " session"
                    @if sessions_today.len() != 1 { "s" }
                    @if let Some(email) = author_filter {
                        " · author=" (email)
                    }
                    ")"
                }
            }
            div class="px-4 pb-4 pt-2 text-xs leading-relaxed flex flex-col gap-3" {
                @if by_repo.is_empty() && sessions_today.is_empty()
                    && dirty.is_empty() && failing_ci.is_empty()
                {
                    p class="text-[#574f7d]/70" { "nothing to report — clean slate." }
                }

                @if !by_repo.is_empty() {
                    div {
                        div class="font-bold text-[#3e375d] mb-1" { "commits (last 24h)" }
                        ul class="list-none p-0 m-0 flex flex-col gap-1.5" {
                            @for (repo_name, cs) in &by_repo {
                                li {
                                    span class="font-semibold" { (repo_name) }
                                    span class="text-[#574f7d]/70" {
                                        " — " (cs.len()) " commit"
                                        @if cs.len() != 1 { "s" }
                                    }
                                    ul class="list-none pl-3 mt-0.5 border-l border-[#9e9fc2]/40
                                              flex flex-col gap-0.5" {
                                        @for c in cs.iter().take(4) {
                                            li class="truncate" { (c.commit.subject) }
                                        }
                                        @if cs.len() > 4 {
                                            li class="italic text-[#574f7d]/60" {
                                                "…and " (cs.len() - 4) " more"
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                @if !sessions_today.is_empty() {
                    div {
                        div class="font-bold text-[#3e375d] mb-1" { "Claude sessions (last 24h)" }
                        ul class="list-none p-0 m-0 flex flex-col gap-0.5" {
                            @for sr in sessions_today.iter().take(6) {
                                li class="truncate" {
                                    @if let Some(s) = &sr.session.summary { (s) }
                                    @else { "(no summary)" }
                                }
                            }
                            @if sessions_today.len() > 6 {
                                li class="italic text-[#574f7d]/60" {
                                    "…and " (sessions_today.len() - 6) " more"
                                }
                            }
                        }
                    }
                }

                @if !dirty.is_empty() || !failing_ci.is_empty() {
                    div {
                        div class="font-bold text-[#3e375d] mb-1" { "open loops" }
                        ul class="list-none p-0 m-0 flex flex-col gap-0.5" {
                            @for r in &failing_ci {
                                li { "CI failing: " span class="font-semibold" { (r.name) } }
                            }
                            @for r in &dirty {
                                li {
                                    "uncommitted work: "
                                    span class="font-semibold" { (r.name) }
                                    span class="text-[#574f7d]/70" {
                                        " (" (r.untracked_files + r.modified_files) " file"
                                        @if (r.untracked_files + r.modified_files) != 1 { "s" }
                                        ")"
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Author-filter toggle. Three states: "all" (default, no query param),
/// "me" (uses detected git email), and per-pill the currently-active one is
/// bolded + underlined. Links not buttons so it's bookmarkable + history-
/// navigable. Only rendered when we actually know the viewer's email.
fn author_toggle(my_email: Option<&str>, active: Option<&str>) -> Markup {
    let Some(me) = my_email else {
        return html! {};
    };
    let is_me = active.map(|e| e == me).unwrap_or(false);
    let is_all = active.is_none();
    let base = "text-[11px] px-2 py-1 rounded-md border transition-colors";
    let active_cls = "bg-[#574f7d] text-white border-[#3e375d] font-semibold";
    let inactive_cls = "bg-transparent text-[#574f7d] border-[#9e9fc2]/50 hover:bg-[#9e9fc2]/15";
    html! {
        div class="flex items-center gap-1 ml-auto mr-2"
            title={ "author filter — currently " (active.unwrap_or("all")) } {
            span class="text-[10px] uppercase tracking-[0.08em] text-[#9e9fc2] font-bold mr-1" {
                "author"
            }
            a href="?author=all"
              class={ (base) " " @if is_all { (active_cls) } @else { (inactive_cls) } } {
                "all"
            }
            a href="?author=me"
              class={ (base) " " @if is_me { (active_cls) } @else { (inactive_cls) } }
              title=(me) {
                "me"
            }
        }
    }
}

/// Countdown to the next auto-refresh tick. Server-side renders the initial
/// label + a `data-deadline-unix` attribute; static/refresh-countdown.js
/// decrements the visible text once a second. Hidden when periodic refresh
/// is disabled (`REPO_RECALL_REFRESH_INTERVAL_SECS=0`). Before the first scan
/// completes (`last_scan = None`) we don't know the anchor, so we show
/// "scanning…" with no deadline.
fn next_refresh_countdown(
    interval_secs: u64,
    last_scan: Option<chrono::DateTime<chrono::Utc>>,
) -> Markup {
    if interval_secs == 0 {
        return html! {};
    }
    let label_class = "text-[11px] uppercase tracking-[0.08em] text-[#9e9fc2] font-bold";
    let value_class = "text-sm text-[#574f7d] mt-1 font-mono";
    let Some(scan) = last_scan else {
        return html! {
            div {
                div class=(label_class) { "next refresh" }
                div class=(value_class) { "scanning…" }
            }
        };
    };
    let deadline = scan.timestamp() + interval_secs as i64;
    html! {
        div {
            div class=(label_class) {
                "next refresh"
                span class="ml-1 lowercase tracking-normal font-normal text-[#574f7d]/60" {
                    "(every " (format_secs(interval_secs)) ")"
                }
            }
            div id="next-refresh-countdown"
                class=(value_class)
                data-deadline-unix=(deadline) {
                "—"
            }
        }
        script src="/static/refresh-countdown.js" defer {}
    }
}

/// Compact "1m 30s" / "30s" / "2m" formatting for short durations.
fn format_secs(s: u64) -> String {
    if s < 60 {
        return format!("{s}s");
    }
    let m = s / 60;
    let r = s % 60;
    if r == 0 {
        format!("{m}m")
    } else {
        format!("{m}m {r}s")
    }
}

/// Prose form of one action-required item. The format is "what's wrong"
/// followed by "what to do about it" and a couple of pointer links - the
/// repo's dashboard page (always) plus a context-appropriate remote deep
/// link (Actions tab for CI, PR review queue for review-requested, etc.).
/// Kept verbose-on-purpose: the collapsed pill row is the at-a-glance view,
/// this is the "I expanded it because I want to actually act on something"
/// view.
fn action_sentence(signal: &str, r: &db::Repo, detail: &str) -> Markup {
    let repo_link = html! {
        a class="font-semibold underline decoration-white/40 hover:decoration-white text-white"
          href={ "/repos/" (r.id) } { (r.name) }
    };
    let remote_link = |suffix: &str, label: &str| -> Markup {
        match &r.remote_url {
            Some(url) => html! {
                " "
                a class="text-white/80 underline decoration-white/30 hover:decoration-white
                         hover:text-white"
                  target="_blank" rel="noopener" href={ (url) (suffix) } { (label) " ↗" }
            },
            None => html! {},
        }
    };
    match signal {
        "ci_failing" => html! {
            "CI is failing on the default branch of " (repo_link) ". "
            "Open the failing run, fix the breakage, and push a green commit before merging anything else."
            (remote_link("/actions", "actions"))
        },
        "review_requested" => html! {
            (detail) " in " (repo_link) ". "
            "Read each PR and approve, request changes, or comment so the author isn't blocked on you."
            (remote_link("/pulls/review-requested/@me", "review queue"))
            @if !r.review_requested_pr_files.is_empty() {
                ul class="mt-2 space-y-1 list-none p-0" {
                    @for pr in &r.review_requested_pr_files {
                        li class="text-[12px] text-white/85" {
                            @match &r.remote_url {
                                Some(url) => a class="font-mono text-white underline decoration-white/40 hover:decoration-white"
                                              target="_blank" rel="noopener"
                                              href={ (url) "/pull/" (pr.number) } { "#" (pr.number) },
                                None => span class="font-mono text-white" { "#" (pr.number) },
                            }
                            @if pr.files.is_empty() {
                                " (no files)"
                            } @else {
                                " "
                                @for (i, f) in pr.files.iter().enumerate() {
                                    @if i > 0 { ", " }
                                    code class="font-mono text-[11px] text-white/80 bg-white/10 px-1 rounded" {
                                        (f)
                                    }
                                }
                            }
                        }
                    }
                }
            }
        },
        "issue_assigned" => html! {
            (detail) " in " (repo_link) ". "
            "Triage each one — close, comment, or pick the next action — so the queue reflects what's actually live."
            (remote_link("/issues/assigned/@me", "issue queue"))
        },
        "deploy_failing" => html! {
            (detail) " in " (repo_link) ". "
            "Open the failing run, fix the breakage, and push a green commit before merging anything else."
            (remote_link("/actions", "actions"))
        },
        "deploy_stale" => html! {
            (detail) " in " (repo_link) ". "
            "The deploy path itself may have rotted — kick a manual run or check for missing triggers."
            (remote_link("/actions", "actions"))
        },
        "in_progress_op" => html! {
            (repo_link) " has " (detail) ". "
            "Finish it (`git "
            (r.in_progress_op.as_deref().unwrap_or("op"))
            " --continue`) or abort it (`git "
            (r.in_progress_op.as_deref().unwrap_or("op"))
            " --abort`) before starting any new git work in this repo."
        },
        "detached_head" => html! {
            (repo_link) " is on a detached HEAD. "
            "Pick a branch (`git switch <branch>`) or create one (`git switch -c <name>`) "
            "before committing, otherwise any new commits will be unreachable."
        },
        "dirty_tree" => html! {
            (repo_link) " has " (detail) ". "
            "Review the diff, then commit, stash, or discard before the next refresh or deploy. "
            "Path: " code class="font-mono text-[11px] text-white/80 bg-white/10 px-1 rounded" {
                (r.path)
            }
        },
        _ => html! { (repo_link) " - " (detail) },
    }
}

fn stat(label: &str, value: &str) -> Markup {
    html! {
        div {
            div class="text-[11px] uppercase tracking-[0.08em] text-[#9e9fc2] font-bold" { (label) }
            div class="text-2xl font-bold text-[#3e375d] leading-none mt-1" { (value) }
        }
    }
}

fn render_repos(repos: &[db::Repo], scan_cwd: &Path) -> Markup {
    html! {
        @if repos.is_empty() {
            p class="text-[#574f7d]/70" { "no repos discovered in cwd + configured depth" }
        } @else {
            ul class="list-none p-0 m-0" {
                @for r in repos {
                    @let action_required = activity::is_action_required(r);
                    @let signals: Vec<&'static str> =
                        derive_action_signals(r)
                            .into_iter().map(|s| s.signal).collect();
                    @let signal_attr = signals.join(" ");
                    li class={
                            (LI)
                            @if activity::is_dormant(r) { " opacity-40" }
                        }
                        data-repo-id=(r.id)
                        data-repo-name=(r.name)
                        data-action-required=(if action_required { "true" } else { "false" })
                        data-signals=(signal_attr) {
                        div class=(ROW) {
                            span class="font-semibold" {
                                a class=(LINK) href={ "/repos/" (r.id) } {
                                    (display_name(r, scan_cwd))
                                }
                            }
                            @if r.session_count > 0 {
                                span class=(PILL) { (r.session_count) " sessions" }
                            }
                            @if r.commits_30d > 0 {
                                span class=(PILL) title="commits in the last 30 days" {
                                    (r.commits_30d) " commits (30d)"
                                }
                            }
                            @if r.loc_churn_30d > 0 {
                                span class=(PILL)
                                     title="lines added + deleted in the last 30 days (30-day churn)" {
                                    (compact_count(r.loc_churn_30d)) " churn (30d)"
                                }
                            }
                            @if r.authors_30d > 0 {
                                span class=(PILL)
                                     title="unique commit authors in the last 30 days" {
                                    (r.authors_30d) " authors (30d)"
                                }
                            }
                            @let uncommitted = r.untracked_files + r.modified_files;
                            @if uncommitted > 0 {
                                span class=(PILL_ALERT)
                                     data-flag="dirty_tree"
                                     title={
                                        "working-tree files right now — "
                                        (r.modified_files) " modified + "
                                        (r.untracked_files) " untracked"
                                     } {
                                    (compact_count(uncommitted)) " uncommitted"
                                }
                            }
                            @if let Some(op) = r.in_progress_op.as_deref() {
                                span class=(PILL_ALERT)
                                     data-flag="in_progress_op"
                                     title="a git operation is mid-flight — finish or abort it" {
                                    (op) " in progress"
                                }
                            }
                            @if r.head_ref.as_deref() == Some("detached") {
                                span class=(PILL_ALERT)
                                     data-flag="detached_head"
                                     title="HEAD is detached — not on any branch" {
                                    "detached HEAD"
                                }
                            }
                            @if r.stash_count > 0 {
                                span class=(PILL) title="`git stash list` entries" {
                                    (r.stash_count) " stashed"
                                }
                            }
                            @if r.commits_ahead > 0 {
                                span class=(PILL) title="local commits not on origin yet" {
                                    "↑ " (r.commits_ahead) " unpushed"
                                }
                            }
                            @if r.commits_behind > 0 {
                                span class=(PILL) title="upstream has commits you don't — pull to catch up" {
                                    "↓ " (r.commits_behind) " behind"
                                }
                            }
                            (ci_pill(r))
                            @if r.prs_awaiting_my_review > 0 {
                                span class=(PILL_ALERT)
                                     data-flag="review_requested"
                                     title="PRs where you're a requested reviewer" {
                                    (r.prs_awaiting_my_review) " awaiting your review"
                                }
                            }
                            @if r.prs_mine_no_reviewer > 0 {
                                span class=(PILL_ALERT)
                                     data-flag="pr_no_reviewer"
                                     title="your open PRs with no reviewer requested - request a reviewer or self-merge" {
                                    "↗ " (r.prs_mine_no_reviewer) " yours, no reviewer"
                                }
                            }
                            @if r.issues_assigned_to_me > 0 {
                                span class=(PILL_ALERT)
                                     data-flag="issue_assigned"
                                     title="open issues assigned to you" {
                                    (r.issues_assigned_to_me) " issue"
                                    @if r.issues_assigned_to_me != 1 { "s" }
                                    " assigned"
                                }
                            }
                            @if activity::is_deploy_failing(r) {
                                span class=(PILL_ALERT)
                                     data-flag="deploy_failing"
                                     title={
                                        "deploy workflow `"
                                        (r.deploy_workflow.as_deref().unwrap_or("deploy"))
                                        "` last run failed"
                                     } {
                                    "deploy failing"
                                }
                            } @else if activity::is_deploy_stale(r) {
                                @let days = r.deploy_last_success_ts
                                    .map(|ts| (chrono::Utc::now().timestamp() - ts) / 86_400)
                                    .unwrap_or(0);
                                span class=(PILL_ALERT)
                                     data-flag="deploy_stale"
                                     title={
                                        "deploy workflow `"
                                        (r.deploy_workflow.as_deref().unwrap_or("deploy"))
                                        "` last green " (days) "d ago"
                                     } {
                                    "deploy stale " (days) "d"
                                }
                            }
                            @if r.prs_mine_awaiting_review > 0 {
                                span class=(PILL)
                                     title="your open PRs with a reviewer requested - waiting on them" {
                                    "↗ " (r.prs_mine_awaiting_review) " yours, awaiting reviewer"
                                }
                            }
                            @if r.open_prs > 0 {
                                span class=(PILL) title="total open PRs (including drafts)" {
                                    (r.open_prs) " PRs"
                                    @if r.draft_prs > 0 {
                                        " (" (r.draft_prs) " draft)"
                                    }
                                }
                            }
                            @if r.open_issues > 0 {
                                span class=(PILL) title="total open issues" {
                                    (r.open_issues) " issues"
                                }
                            }
                        }
                        @if let Some(url) = remote_link(r) {
                            div class="text-xs mt-0.5" {
                                a class=(LINK) href=(url.0) target="_blank" rel="noopener" {
                                    (url.1)
                                }
                            }
                        }
                        div class=(PATH) { (r.path) }
                    }
                }
            }
        }
    }
}

/// Render the "active on GitHub, not cloned" panel — one row per repo with a
/// clone button that posts to `/api/clone`. Each row is keyed by a slug of
/// the `owner/name` so the post-clone fragment knows which row to swap.
fn render_uncloned(repos: &[db::ActiveRemoteRepo]) -> Markup {
    html! {
        ul class="list-none p-0 m-0" {
            @for r in repos {
                li class=(LI) {
                    div class=(ROW) {
                        @if !crate::routes::templates::is_demo_mode() {
                            div id={ "clone-row-" (crate::routes::actions::slugify(&r.full_name)) }
                                class="flex items-baseline gap-2 shrink-0" {
                                form hx-post="/api/clone"
                                     hx-target={ "#clone-row-" (crate::routes::actions::slugify(&r.full_name)) }
                                     hx-swap="outerHTML"
                                     class="contents" {
                                    input type="hidden" name="full_name" value=(r.full_name);
                                    button type="submit"
                                        class="bg-[#574f7d] text-white px-2 py-0.5 rounded text-[10px]
                                               font-bold tracking-wide hover:bg-[#3e375d]
                                               transition-colors cursor-pointer shadow-sm" {
                                        "clone ↓"
                                    }
                                }
                            }
                        }
                        a class={ (LINK) " font-semibold" }
                          href=(r.https_url) target="_blank" rel="noopener" {
                            (r.full_name)
                        }
                        @if let Some(branch) = &r.default_branch {
                            span class={ (META) " font-mono" } { (branch) }
                        }
                        @if r.is_fork {
                            span class=(PILL) { "fork" }
                        }
                        @if let Some(ts) = r.pushed_at {
                            span class=(PILL) title="last pushed (GitHub)" {
                                "pushed " (relative_time(Some(ts)))
                            }
                        }
                    }
                    @if let Some(d) = &r.description {
                        p class={ (META) " mt-0.5" } { (d) }
                    }
                }
            }
        }
    }
}

/// "needs push" / "needs pull" panel rows. One row per repo with an inline
/// button that posts to `/api/repos/{id}/{action}`. Mirrors the clone panel:
/// the button's container has a stable id so the htmx swap can replace just
/// that cell with the result fragment from `actions.rs`.
fn render_needs_action(repos: &[&db::Repo], action: &str) -> Markup {
    let arrow = if action == "push" { "↑" } else { "↓" };
    html! {
        ul class="list-none p-0 m-0" {
            @for r in repos {
                @let count = if action == "push" { r.commits_ahead } else { r.commits_behind };
                @let target_id = format!("repo-action-{action}-{}", r.id);
                li class=(LI) {
                    div class=(ROW) {
                        div id=(target_id) class="flex items-baseline gap-2 shrink-0" {
                            form hx-post={ "/api/repos/" (r.id) "/" (action) }
                                 hx-target={ "#" (target_id) }
                                 hx-swap="outerHTML"
                                 class="contents" {
                                button type="submit"
                                    class="bg-[#574f7d] text-white px-2 py-0.5 rounded text-[10px]
                                           font-bold tracking-wide hover:bg-[#3e375d]
                                           transition-colors cursor-pointer shadow-sm" {
                                    (action) " " (arrow)
                                }
                            }
                        }
                        a class={ (LINK) " font-semibold" } href={ "/repos/" (r.id) } {
                            (r.name)
                        }
                        span class=(PILL) {
                            (arrow) " " (count)
                            @if action == "push" { " unpushed" } @else { " behind" }
                        }
                        @if let Some(branch) = &r.head_ref {
                            @if branch != "detached" {
                                span class={ (META) " font-mono" } { (branch) }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn render_uncommitted_groups(groups: &[db::UncommittedGroup]) -> Markup {
    html! {
        @if groups.is_empty() {
            p class="text-[#574f7d]/70 text-xs" {
                "nothing dirty — every working tree is clean"
            }
        } @else {
            ul class="list-none p-0 m-0 flex flex-col gap-3" {
                @for g in groups {
                    li {
                        // Per-repo header row: repo name + total count pill.
                        div class="flex items-baseline gap-2 flex-wrap" {
                            a class={ (LINK) " font-semibold" } href={ "/repos/" (g.repo_id) } {
                                (g.repo_name)
                            }
                            span class=(PILL) {
                                (g.total) " file"
                                @if g.total != 1 { "s" }
                            }
                        }
                        // Sampled file paths (mod first, then untracked).
                        ul class="list-none pl-3 mt-1 border-l border-[#9e9fc2]/40 \
                                  flex flex-col gap-0.5" {
                            @for (path, kind) in &g.sample {
                                li class="flex gap-2 items-baseline" {
                                    span class={
                                        "text-[10px] uppercase tracking-[0.04em] font-bold \
                                         shrink-0 w-7 "
                                        @if kind == "untracked" { "text-[#9192bb]" }
                                        @else { "text-[#574f7d]" }
                                    } {
                                        @if kind == "untracked" { "new" } @else { "mod" }
                                    }
                                    span class="font-mono text-[11px] break-all" { (path) }
                                }
                            }
                            // "…and 3 more" — the DB query capped the sample, but
                            // `total` is the true count, so we can show what's
                            // hidden without refetching.
                            @let shown = g.sample.len() as i64;
                            @if g.total > shown {
                                li class="text-[11px] text-[#574f7d]/60 italic" {
                                    "…and " (g.total - shown) " more"
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn render_ci_failures(failures: &[db::CiFailure]) -> Markup {
    html! {
        ul class="list-none p-0 m-0" {
            @for f in failures {
                li class=(LI) {
                    div class="flex items-baseline gap-2 flex-wrap" {
                        a class={ (LINK) " font-semibold text-base" } href={ "/repos/" (f.repo_id) } {
                            (f.repo_name)
                        }
                        @if let Some(branch) = f.default_branch.as_deref() {
                            span class={ (PILL) " font-mono" } title="default branch" { (branch) }
                        }
                        @if let (Some(url), Some(branch)) =
                            (f.remote_url.as_deref(), f.default_branch.as_deref())
                        {
                            a class=(PILL_ALERT)
                              href={ (url) "/actions?query=branch%3A" (branch) }
                              target="_blank" rel="noopener"
                              title="open the failing branch's Actions history on GitHub" {
                                "view failing CI ↗"
                            }
                        }
                    }
                }
            }
        }
    }
}

fn render_commits(commits: &[db::CommitWithRepo], scan_cwd: &Path) -> Markup {
    html! {
        @if commits.is_empty() {
            p class="text-[#574f7d]/70" { "no commits indexed yet" }
        } @else {
            ul class="list-none p-0 m-0" {
                @for c in commits {
                    li class=(LI) {
                        div class=(ROW) {
                            (short_sha(&c.commit.sha, c.repo_remote_url.as_deref()))
                            span class="font-semibold truncate" { (c.commit.subject) }
                        }
                        div class={ (ROW) " " (META) } {
                            span { (relative_time(Some(c.commit.timestamp))) }
                            a class=(PILL) href={ "/repos/" (c.repo_id) } {
                                (display_repo_label(&c.repo_name, &c.repo_path, scan_cwd))
                            }
                            span { (c.commit.author_name) }
                        }
                    }
                }
            }
        }
    }
}

/// Short-SHA chip. Renders as an external link to `{remote}/commit/<sha>` if
/// we know the remote; plain `<code>` otherwise. GitHub / GitLab / Bitbucket
/// all use the same `/commit/<sha>` path, so the link works without
/// host-specific branching.
fn short_sha(sha: &str, remote_url: Option<&str>) -> Markup {
    let short: &str = &sha[..sha.len().min(7)];
    let chip_class = "font-mono text-[11px] text-[#574f7d] bg-[#9e9fc2]/15 \
                      px-1.5 py-0.5 rounded hover:bg-[#9e9fc2]/30 transition-colors";
    html! {
        @match remote_url {
            Some(url) if !url.is_empty() => {
                a class=(chip_class) href={ (url) "/commit/" (sha) }
                  target="_blank" rel="noopener" title=(sha) {
                    (short)
                }
            }
            _ => {
                code class="font-mono text-[11px] text-[#9e9fc2] bg-[#9e9fc2]/15 px-1.5 py-0.5 rounded"
                     title=(sha) {
                    (short)
                }
            }
        }
    }
}

/// CI status pill — only rendered for actionable states. "success" and
/// "unknown" / missing stay silent to keep the row chrome-free. "failure"
/// uses `PILL_ALERT` so a broken default branch jumps out of the list;
/// "running" / "pending" use the dashed faint variant so they read as
/// transient.
fn ci_pill(r: &db::Repo) -> Markup {
    let Some(status) = r.ci_status.as_deref() else {
        return html! {};
    };
    let default_branch = r.default_branch.as_deref().unwrap_or("");
    let href = r
        .remote_url
        .as_deref()
        .filter(|_| !default_branch.is_empty())
        .map(|u| format!("{u}/actions?query=branch%3A{default_branch}"));
    let (class, text, title, flag) = match status {
        "failure" => (
            PILL_ALERT,
            "CI failing",
            "latest default-branch CI run failed",
            Some("ci_failing"),
        ),
        "running" => (
            PILL_FAINT,
            "CI running",
            "default-branch CI currently running",
            None,
        ),
        "pending" => (
            PILL_FAINT,
            "CI pending",
            "default-branch CI is queued / waiting",
            None,
        ),
        _ => return html! {}, // success / unknown — stay silent
    };
    let flag_attr = flag.unwrap_or("");
    html! {
        @match href {
            Some(h) => {
                a class=(class) href=(h) target="_blank" rel="noopener"
                  data-flag=(flag_attr) title=(title) { (text) }
            }
            None => {
                span class=(class) data-flag=(flag_attr) title=(title) { (text) }
            }
        }
    }
}

/// `(href, text)` for a repo's default-branch remote link. Returns `None` if
/// we don't know the origin URL. When the default branch is known, the href
/// points at `/tree/<branch>` and the text strips `https://` + appends
/// ` @ <branch>` so the link reads like
/// `github.com/coilysiren/backend @ main`. When we have a URL but no branch,
/// the href is the bare URL and the text is the bare host + path.
fn remote_link(r: &db::Repo) -> Option<(String, String)> {
    let base = r.remote_url.as_ref()?;
    let display = base
        .strip_prefix("https://")
        .or_else(|| base.strip_prefix("http://"))
        .unwrap_or(base);
    match r.default_branch.as_deref() {
        Some(branch) if !branch.is_empty() => Some((
            format!("{base}/tree/{branch}"),
            format!("{display} @ {branch}"),
        )),
        _ => Some((base.clone(), display.to_string())),
    }
}

/// Compute a cwd-relative label from a (name, path) pair. Mirrors
/// `display_name(&Repo, ...)` but takes the primitives already returned by our
/// join queries, so callers don't need to reconstruct a `db::Repo`.
fn display_repo_label(name: &str, path: &str, scan_cwd: &Path) -> String {
    let p = Path::new(path);
    match p.strip_prefix(scan_cwd) {
        Ok(rel) if !rel.as_os_str().is_empty() => rel.display().to_string(),
        _ => name.to_string(),
    }
}

fn render_sessions(sessions: &[db::SessionWithRepos]) -> Markup {
    html! {
        @if sessions.is_empty() {
            p class="text-[#574f7d]/70" { "no sessions indexed yet" }
        } @else {
            ul class="list-none p-0 m-0" {
                @for sr in sessions {
                    li class=(LI) {
                        div class=(ROW) {
                            a class={ (LINK) " font-semibold" } href={ "/sessions/" (sr.session.id) } {
                                @if let Some(s) = &sr.session.summary { (s) }
                                @else { "(no summary)" }
                            }
                        }
                        div class={ (ROW) " " (META) } {
                            span { (relative_time(sr.session.started_at)) }
                            span { (sr.session.message_count) " msgs" }
                            @for (rid, name, _path) in &sr.repos {
                                a class=(PILL) href={ "/repos/" (rid) } { (name) }
                            }
                        }
                    }
                }
            }
        }
    }
}
