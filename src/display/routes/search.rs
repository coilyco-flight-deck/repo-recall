use std::sync::atomic::Ordering;

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use maud::html;
use serde::{Deserialize, Serialize};

use crate::db;
use crate::display::routes::negotiate::{json_with_etag, wants_json};
use crate::display::routes::templates::{page, H2, LI, LINK, PANEL, PATH, ROW};
use crate::AppState;

fn to_db_hit(h: crate::search::SearchHit) -> db::SearchHit {
    db::SearchHit {
        kind: h.kind,
        ref_id: h.ref_id,
        text: h.text,
        extra: None,
    }
}

#[derive(Debug, Deserialize, Default)]
pub struct SearchParams {
    #[serde(default)]
    pub q: Option<String>,
    #[serde(default)]
    pub format: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct SearchJson {
    query: String,
    repos: Vec<db::SearchHit>,
    sessions: Vec<db::SearchHit>,
    commits: Vec<db::SearchHit>,
    scan_version: u64,
}

pub async fn search(
    State(state): State<AppState>,
    Query(params): Query<SearchParams>,
    headers: HeaderMap,
) -> Response {
    let q = params.q.unwrap_or_default();
    let q_trimmed = q.trim().to_string();

    let hits: Vec<db::SearchHit> = if q_trimmed.is_empty() {
        Vec::new()
    } else {
        match state.search_index.search(&q_trimmed, 100) {
            Ok(hits) => hits.into_iter().map(to_db_hit).collect(),
            Err(e) => {
                tracing::debug!("tantivy search failed for {q_trimmed:?}: {e:?}");
                Vec::new()
            }
        }
    };

    // Partition by kind so we can render three focused sections.
    let mut repos: Vec<&db::SearchHit> = Vec::new();
    let mut sessions: Vec<&db::SearchHit> = Vec::new();
    let mut commits: Vec<&db::SearchHit> = Vec::new();
    for h in &hits {
        match h.kind.as_str() {
            "repo" => repos.push(h),
            "session" => sessions.push(h),
            "commit" => commits.push(h),
            _ => {}
        }
    }

    if wants_json(&headers, params.format.as_deref()) {
        let v = state.scan_version.load(Ordering::Acquire);
        let body = SearchJson {
            query: q_trimmed.clone(),
            repos: repos.iter().map(|h| (*h).clone()).collect(),
            sessions: sessions.iter().map(|h| (*h).clone()).collect(),
            commits: commits.iter().map(|h| (*h).clone()).collect(),
            scan_version: v,
        };
        return json_with_etag(&headers, v, &body);
    }

    let body = html! {
        form method="get" action="/search" class="flex gap-2 mb-4" {
            input name="q" value=(q_trimmed) autofocus
                  placeholder="search repos, sessions, commit subjects…"
                  class="flex-1 px-3 py-2 text-sm rounded-md border border-[#9e9fc2]/50
                         bg-white text-[#3e375d] placeholder:text-[#574f7d]/50
                         focus:outline-none focus:border-[#574f7d]";
            button type="submit"
                   class="bg-[#574f7d] text-white px-4 py-2 rounded-md text-xs font-bold
                          tracking-wide hover:bg-[#3e375d] transition-colors cursor-pointer" {
                "search"
            }
        }

        @if q_trimmed.is_empty() {
            p class="text-[#574f7d]/70 text-xs" {
                "type a query above. matches against repo names + paths, session summaries, and commit subjects. case-insensitive, with porter stemming (so \"refactor\" matches \"refactoring\")."
            }
        } @else if hits.is_empty() {
            p class="text-[#574f7d]/70" {
                "no matches for " code class="font-mono" { (q_trimmed) } "."
            }
        } @else {
            div class="flex flex-col gap-4" {
                @if !repos.is_empty() {
                    section class=(PANEL) {
                        h2 class=(H2) { "repos (" (repos.len()) ")" }
                        ul class="list-none p-0 m-0" {
                            @for h in &repos {
                                li class=(LI) {
                                    a class={ (LINK) " font-semibold" }
                                      href={ "/repos/" (h.ref_id) } {
                                        (h.text)
                                    }
                                }
                            }
                        }
                    }
                }
                @if !sessions.is_empty() {
                    section class=(PANEL) {
                        h2 class=(H2) { "sessions (" (sessions.len()) ")" }
                        ul class="list-none p-0 m-0" {
                            @for h in &sessions {
                                li class=(LI) {
                                    a class={ (LINK) " font-semibold" }
                                      href={ "/sessions/" (h.ref_id) } {
                                        (h.text)
                                    }
                                }
                            }
                        }
                    }
                }
                @if !commits.is_empty() {
                    section class=(PANEL) {
                        h2 class=(H2) { "commits (" (commits.len()) ")" }
                        ul class="list-none p-0 m-0" {
                            @for h in &commits {
                                li class=(LI) {
                                    div class=(ROW) {
                                        span class="font-semibold" { (h.text) }
                                    }
                                    div class=(PATH) { "commit id " (h.ref_id) }
                                }
                            }
                        }
                    }
                }
            }
        }
    };
    page("search", body).into_response()
}
