use std::sync::atomic::Ordering;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use maud::html;
use serde::{Deserialize, Serialize};

use crate::db;
use crate::routes::negotiate::{json_with_etag, wants_json};
use crate::routes::templates::{page, relative_time, H2, LI, LINK, META, PANEL, PATH, ROW};
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
        Ok((repo, sessions, commits, hotspots))
    })
    .await
    .unwrap();

    let (repo, sessions, commits, hotspots) = match data {
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
            scan_version: v,
        };
        return json_with_etag(&headers, v, &body);
    }

    let body = html! {
        h1 class="text-lg font-semibold mb-1" { (repo.name) }
        p class=(PATH) { (repo.path) }
        section class="mt-3 flex items-center gap-2 flex-wrap" {
            @if !crate::routes::templates::is_demo_mode() {
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
            }
            div id={ "repo-action-pull-" (repo.id) } {}
            div id={ "repo-action-push-" (repo.id) } {}
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
                                    @if let Some(sum) = &s.summary { (sum) }
                                    @else { "(no summary)" }
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
