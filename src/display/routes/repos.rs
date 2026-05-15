use std::sync::atomic::Ordering;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use maud::html;
use serde::{Deserialize, Serialize};

use crate::db;
use crate::display::routes::negotiate::{json_with_etag, wants_json};
use crate::display::routes::templates::{
    page, relative_time, H2, LI, LINK, META, PANEL, PATH, ROW,
};
use crate::AppState;

#[derive(Debug, Deserialize, Default)]
pub struct DetailParams {
    #[serde(default)]
    pub format: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct RepoDetailJson {
    repo: db::Repo,
    sessions: Vec<db::Session>,
    commits: Vec<db::Commit>,
    hotspots: Vec<db::FileHotspot>,
    /// Parsed write-once dispatch records under `docs/repo-dispatch/`
    /// for this repo (#92, #113, #117). Newest-first.
    dispatches: Vec<db::DispatchRow>,
    /// Open structural-ask issues filed against this repo (#114, #117).
    structural_asks: Vec<db::LabeledIssueRow>,
    scan_version: u64,
}

pub async fn detail(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Query(params): Query<DetailParams>,
    headers: HeaderMap,
) -> Response {
    let state2 = state.clone();
    let data = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        let cache = &state2.cache_db;
        let repo = cache.get_repo(id)?;
        let sessions = if repo.is_some() {
            cache.sessions_for_repo(id)?
        } else {
            Vec::new()
        };
        let commits = if repo.is_some() {
            cache.commits_for_repo(id, 50)?
        } else {
            Vec::new()
        };
        let cutoff_30d = chrono::Utc::now().timestamp() - 30 * 86_400;
        let hotspots = if repo.is_some() {
            cache.file_hotspots(id, cutoff_30d, 10)?
        } else {
            Vec::new()
        };
        let dispatches = if repo.is_some() {
            cache.dispatches_for_repo(id)?
        } else {
            Vec::new()
        };
        let structural_asks = if repo.is_some() {
            cache.labeled_issues_for_repo(id, "structural-ask")?
        } else {
            Vec::new()
        };
        Ok((
            repo,
            sessions,
            commits,
            hotspots,
            dispatches,
            structural_asks,
        ))
    })
    .await
    .unwrap();

    let (repo, sessions, commits, hotspots, dispatches, structural_asks) = match data {
        Ok(d) => d,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                page("error", html! { p { (e.to_string()) } }),
            )
                .into_response();
        }
    };

    let Some(repo) = repo else {
        return (
            StatusCode::NOT_FOUND,
            page("not found", html! { p { "repo not found" } }),
        )
            .into_response();
    };

    if wants_json(&headers, params.format.as_deref()) {
        let v = state.scan_version.load(Ordering::Acquire);
        let body = RepoDetailJson {
            repo,
            sessions,
            commits,
            hotspots,
            dispatches,
            structural_asks,
            scan_version: v,
        };
        return json_with_etag(&headers, v, &body);
    }

    let body = html! {
        h1 class="text-lg font-semibold mb-1" { (repo.name) }
        p class=(PATH) { (repo.path) }
        section class="mt-3 flex items-center gap-2 flex-wrap" {
            form hx-post={ "/api/repos/" (repo.id) "/pull" }
                 hx-target={ "#repo-action-pull-" (repo.id) }
                 hx-swap="outerHTML" {
                button type="submit"
                    class="bg-[#574f7d] text-white px-3 py-1.5 rounded-md text-xs font-bold tracking-wide
                           hover:bg-[#3e375d] transition-colors cursor-pointer shadow-sm" {
                    "↓ git pull"
                }
            }
            form hx-post={ "/api/repos/" (repo.id) "/push" }
                 hx-target={ "#repo-action-push-" (repo.id) }
                 hx-swap="outerHTML" {
                button type="submit"
                    class="bg-[#574f7d] text-white px-3 py-1.5 rounded-md text-xs font-bold tracking-wide
                           hover:bg-[#3e375d] transition-colors cursor-pointer shadow-sm" {
                    "↑ git push"
                }
            }
            div id={ "repo-action-pull-" (repo.id) } {}
            div id={ "repo-action-push-" (repo.id) } {}
        }
        @if !dispatches.is_empty() {
            section class={ (PANEL) " mt-4" } {
                h2 class=(H2) { "repo-dispatch (" (dispatches.len()) ")" }
                ul class="list-none p-0 m-0" {
                    @for d in &dispatches {
                        li class=(LI) {
                            div class=(ROW) {
                                span class="font-mono text-[11px] break-all font-semibold" { (d.file_path) }
                            }
                            div class={ (ROW) " " (META) } {
                                @if let Some(ts) = d.dispatched_at { span { (relative_time(Some(ts))) } }
                                @if let Some(s) = d.score { span { "score " (s) } }
                                @if let Some(c) = d.autonomy_confidence {
                                    span { "autonomy " (c) "/5" }
                                }
                                @if let Some((owner, repo_name, n)) = &d.tracking_issue {
                                    span {
                                        a class=(LINK)
                                          href={ "https://github.com/" (owner) "/" (repo_name) "/issues/" (n) }
                                          target="_blank" rel="noopener" {
                                            "tracking #" (n)
                                        }
                                    }
                                }
                            }
                            @if !d.issue_refs.is_empty() {
                                div class={ (ROW) " " (META) } {
                                    @for (o, r, n) in &d.issue_refs {
                                        a class=(LINK)
                                          href={ "https://github.com/" (o) "/" (r) "/issues/" (n) }
                                          target="_blank" rel="noopener" {
                                            (o) "/" (r) "#" (n)
                                        }
                                    }
                                }
                            }
                            @if let Some(basis) = &d.autonomy_confidence_basis {
                                div class="text-[11px] text-[#574f7d]/70 italic mt-0.5" { (basis) }
                            }
                        }
                    }
                }
            }
        }
        @if !structural_asks.is_empty() {
            section class={ (PANEL) " mt-4" } {
                h2 class=(H2) { "open structural-asks (" (structural_asks.len()) ")" }
                ul class="list-none p-0 m-0" {
                    @for a in &structural_asks {
                        li class=(LI) {
                            div class=(ROW) {
                                @match repo.remote_url.as_deref() {
                                    Some(url) if !url.is_empty() => {
                                        a class={ (LINK) " font-semibold" }
                                          href={ (url) "/issues/" (a.number) }
                                          target="_blank" rel="noopener" {
                                            "#" (a.number) " " (a.title)
                                        }
                                    }
                                    _ => {
                                        span class="font-semibold" { "#" (a.number) " " (a.title) }
                                    }
                                }
                            }
                            div class={ (ROW) " " (META) } {
                                span { (relative_time(Some(a.created_at))) }
                            }
                        }
                    }
                }
            }
        }
        @if !hotspots.is_empty() {
            section class={ (PANEL) " mt-4" } {
                h2 class=(H2) { "hotspots — most-churned files (last 30d, top " (hotspots.len()) ")" }
                ul class="list-none p-0 m-0" {
                    @for h in &hotspots {
                        li class=(LI) {
                            div class=(ROW) {
                                span class="font-mono text-[11px] break-all" { (h.file_path) }
                            }
                            div class={ (ROW) " " (META) } {
                                span { (h.churn) " LOC churn" }
                                span { (h.commits) " commit"
                                    @if h.commits != 1 { "s" }
                                }
                                span { (h.authors) " author"
                                    @if h.authors != 1 { "s" }
                                }
                            }
                        }
                    }
                }
            }
        }
        section class={ (PANEL) " mt-4" } {
            h2 class=(H2) { "sessions (" (sessions.len()) ")" }
            @if sessions.is_empty() {
                p class="text-[#574f7d]/70" { "no sessions joined to this repo yet" }
            } @else {
                ul class="list-none p-0 m-0" {
                    @for s in &sessions {
                        li class=(LI) {
                            div class=(ROW) {
                                a class={ (LINK) " font-semibold" } href={ "/sessions/" (s.id) } {
                                    @if let Some(sum) = &s.last_prompt { (sum) }
                                    @else { "(no prompt)" }
                                }
                            }
                            div class={ (ROW) " " (META) } {
                                span { (relative_time(s.started_at)) }
                                span { (s.message_count) " msgs" }
                            }
                        }
                    }
                }
            }
        }
        section class={ (PANEL) " mt-4" } {
            h2 class=(H2) { "commits (" (commits.len()) ")" }
            @if commits.is_empty() {
                p class="text-[#574f7d]/70" { "no commits indexed yet" }
            } @else {
                ul class="list-none p-0 m-0" {
                    @for c in &commits {
                        li class=(LI) {
                            div class=(ROW) {
                                @match repo.remote_url.as_deref() {
                                    Some(url) if !url.is_empty() => {
                                        a class="font-mono text-[11px] text-[#574f7d] bg-[#9e9fc2]/15 px-1.5 py-0.5 rounded hover:bg-[#9e9fc2]/30 transition-colors"
                                            href={ (url) "/commit/" (c.sha) }
                                            target="_blank" rel="noopener"
                                            title=(c.sha) {
                                            (&c.sha[..c.sha.len().min(7)])
                                        }
                                    }
                                    _ => {
                                        code class="font-mono text-[11px] text-[#9e9fc2] bg-[#9e9fc2]/15 px-1.5 py-0.5 rounded"
                                             title=(c.sha) {
                                            (&c.sha[..c.sha.len().min(7)])
                                        }
                                    }
                                }
                                span class="font-semibold" { (c.subject) }
                            }
                            div class={ (ROW) " " (META) } {
                                span { (relative_time(Some(c.timestamp))) }
                                span { (c.author_name) }
                            }
                        }
                    }
                }
            }
        }
    };
    page(&repo.name, body).into_response()
}
