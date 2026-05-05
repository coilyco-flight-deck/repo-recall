//! User-triggered git/gh side-effects: `git push`, `git pull`, `gh repo clone`.
//! Each handler returns a small HTML fragment so the dashboard / repo detail
//! page can swap the result inline via htmx without a full reload.
//!
//! - `POST /api/repos/{id}/push` runs `git -C <path> push` ([issue #18]).
//! - `POST /api/repos/{id}/pull` runs `git -C <path> pull --ff-only` ([issue #18]).
//! - `POST /api/clone` runs `gh repo clone <full_name>` into the scan cwd ([issue #16]).
//!
//! [issue #16]: https://github.com/coilysiren/repo-recall/issues/16
//! [issue #18]: https://github.com/coilysiren/repo-recall/issues/18

use std::path::PathBuf;
use std::process::Command;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use maud::html;
use serde::Deserialize;

use crate::routes::templates::{LINK, PILL, PILL_ALERT};
use crate::AppState;

#[derive(Debug, Deserialize)]
pub struct CloneForm {
    pub full_name: String,
}

/// `POST /api/repos/{id}/push` — `git push` in the repo's working tree.
pub async fn push(State(state): State<AppState>, Path(id): Path<i64>) -> Response {
    if state.demo_mode {
        return demo_disabled_fragment("git push");
    }
    let path = match repo_path(&state, id).await {
        Ok(Some(p)) => p,
        Ok(None) => return error_fragment("repo not found", id, "push"),
        Err(e) => return error_fragment(&format!("db error: {e}"), id, "push"),
    };
    let out = tokio::task::spawn_blocking(move || run_git(&path, &["push"])).await;
    let result = match out {
        Ok(r) => r,
        Err(e) => GitOutput::failure(format!("task join error: {e}")),
    };
    bump_scan_after(&state).await;
    result_fragment(id, "push", "git push", result)
}

/// `POST /api/repos/{id}/pull` — `git pull --ff-only`. ff-only avoids
/// conjuring an unintentional merge commit when the local branch has diverged.
pub async fn pull(State(state): State<AppState>, Path(id): Path<i64>) -> Response {
    if state.demo_mode {
        return demo_disabled_fragment("git pull");
    }
    let path = match repo_path(&state, id).await {
        Ok(Some(p)) => p,
        Ok(None) => return error_fragment("repo not found", id, "pull"),
        Err(e) => return error_fragment(&format!("db error: {e}"), id, "pull"),
    };
    let out = tokio::task::spawn_blocking(move || run_git(&path, &["pull", "--ff-only"])).await;
    let result = match out {
        Ok(r) => r,
        Err(e) => GitOutput::failure(format!("task join error: {e}")),
    };
    bump_scan_after(&state).await;
    result_fragment(id, "pull", "git pull --ff-only", result)
}

/// `POST /api/clone` — clones a `full_name` from `active_remote_repos` into
/// the scan cwd. Form-encoded (`full_name=owner/repo`) so the dashboard can
/// hx-post a plain `<form>`. Returns a status fragment that replaces the
/// clone button.
pub async fn clone_active(
    State(state): State<AppState>,
    axum::extract::Form(form): axum::extract::Form<CloneForm>,
) -> Response {
    if state.demo_mode {
        return demo_disabled_fragment("gh repo clone");
    }
    let full_name = form.full_name.trim().to_string();
    if !valid_full_name(&full_name) {
        return clone_error_fragment(&full_name, "invalid repo name");
    }

    // Verify the slug appears in our snapshot — keeps this endpoint from
    // being a generic `gh` shell. The viewer can only clone repos `gh`
    // already listed for them in the most recent refresh.
    let known = {
        let cache = state.cache_db.clone();
        let fname = full_name.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<bool> {
            Ok(cache.get_active_repo_by_full_name(&fname)?.is_some())
        })
        .await
    };
    match known {
        Ok(Ok(true)) => {}
        Ok(Ok(false)) => return clone_error_fragment(&full_name, "not in active repo list"),
        Ok(Err(e)) => return clone_error_fragment(&full_name, &format!("db error: {e}")),
        Err(e) => return clone_error_fragment(&full_name, &format!("task error: {e}")),
    }

    let cwd = state.cwd.clone();
    let fname_for_task = full_name.clone();
    let result = tokio::task::spawn_blocking(move || -> GitOutput {
        let output = Command::new("gh")
            .arg("repo")
            .arg("clone")
            .arg(&fname_for_task)
            .current_dir(&cwd)
            .output();
        match output {
            Ok(o) if o.status.success() => GitOutput::success(format!(
                "{}{}",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr),
            )),
            Ok(o) => GitOutput::failure(format!(
                "exit {}: {}{}",
                o.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr),
            )),
            Err(e) => GitOutput::failure(format!("spawn failed: {e}")),
        }
    })
    .await
    .unwrap_or_else(|e| GitOutput::failure(format!("task join error: {e}")));

    if result.ok {
        // Best-effort: kick a refresh so the new repo shows up in the next
        // dashboard render. Spawned, not awaited — UX stays snappy.
        let s = state.clone();
        tokio::spawn(async move {
            let _ = crate::routes::refresh::run_refresh(s).await;
        });
    }

    let id_attr = format!("clone-row-{}", slugify(&full_name));
    Html(
        html! {
            div id=(id_attr) class="flex items-baseline gap-2 flex-wrap" {
                @if result.ok {
                    span class=(PILL) { "cloned ✓ — refreshing…" }
                } @else {
                    span class=(PILL_ALERT) { "clone failed" }
                }
                @if !result.message.trim().is_empty() {
                    code class="font-mono text-[11px] text-[#574f7d]/80 break-all" {
                        (truncate(&result.message, 240))
                    }
                }
            }
        }
        .into_string(),
    )
    .into_response()
}

struct GitOutput {
    ok: bool,
    message: String,
}

impl GitOutput {
    fn success(msg: String) -> Self {
        Self {
            ok: true,
            message: msg,
        }
    }
    fn failure(msg: String) -> Self {
        Self {
            ok: false,
            message: msg,
        }
    }
}

fn run_git(path: &std::path::Path, args: &[&str]) -> GitOutput {
    let Some(path_str) = path.to_str() else {
        return GitOutput::failure("repo path is not valid utf-8".into());
    };
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(path_str);
    for a in args {
        cmd.arg(a);
    }
    match cmd.output() {
        Ok(o) if o.status.success() => GitOutput::success(format!(
            "{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr),
        )),
        Ok(o) => GitOutput::failure(format!(
            "exit {}: {}{}",
            o.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr),
        )),
        Err(e) => GitOutput::failure(format!("spawn failed: {e}")),
    }
}

async fn repo_path(state: &AppState, id: i64) -> anyhow::Result<Option<PathBuf>> {
    let cache = state.cache_db.clone();
    let res = tokio::task::spawn_blocking(move || -> anyhow::Result<Option<PathBuf>> {
        Ok(cache.get_repo(id)?.map(|r| PathBuf::from(r.path)))
    })
    .await?;
    res
}

async fn bump_scan_after(state: &AppState) {
    // Spawn a refresh so the dashboard's pill counts (commits_ahead /
    // commits_behind / dirty tree) reflect the post-push/pull state.
    let s = state.clone();
    tokio::spawn(async move {
        let _ = crate::routes::refresh::run_refresh(s).await;
    });
}

fn result_fragment(id: i64, action: &str, label: &str, result: GitOutput) -> Response {
    let target = format!("repo-action-{action}-{id}");
    Html(
        html! {
            div id=(target) class="flex flex-col gap-1 mt-2" {
                div class="flex items-baseline gap-2 flex-wrap" {
                    @if result.ok {
                        span class=(PILL) { (label) " ✓" }
                    } @else {
                        span class=(PILL_ALERT) { (label) " failed" }
                    }
                    a class=(LINK) href={ "/repos/" (id) } { "reload" }
                }
                @if !result.message.trim().is_empty() {
                    pre class="font-mono text-[11px] text-[#574f7d]/80 bg-[#9e9fc2]/10
                               border border-[#9e9fc2]/30 rounded p-2 whitespace-pre-wrap break-all" {
                        (truncate(&result.message, 1200))
                    }
                }
            }
        }
        .into_string(),
    )
    .into_response()
}

/// 403 used by every host-mutating handler when REPO_RECALL_DEMO=true is in
/// effect. Returns plain text instead of an htmx fragment because the demo
/// dashboard hides the buttons that would surface a fragment - if a request
/// reaches here at all it's an agent or curl probe, not a UI click.
fn demo_disabled_fragment(action: &str) -> Response {
    (
        StatusCode::FORBIDDEN,
        format!("disabled in demo mode: {action}\n"),
    )
        .into_response()
}

fn error_fragment(msg: &str, id: i64, action: &str) -> Response {
    let target = format!("repo-action-{action}-{id}");
    let body = Html(
        html! {
            div id=(target) class="mt-2" {
                span class=(PILL_ALERT) { (msg) }
            }
        }
        .into_string(),
    );
    (StatusCode::OK, body).into_response()
}

fn clone_error_fragment(full_name: &str, msg: &str) -> Response {
    let target = format!("clone-row-{}", slugify(full_name));
    Html(
        html! {
            div id=(target) class="flex items-baseline gap-2 flex-wrap" {
                span class=(PILL_ALERT) { (msg) }
            }
        }
        .into_string(),
    )
    .into_response()
}

fn valid_full_name(s: &str) -> bool {
    if s.is_empty() || s.len() > 200 {
        return false;
    }
    let parts: Vec<&str> = s.split('/').collect();
    if parts.len() != 2 || parts[0].is_empty() || parts[1].is_empty() {
        return false;
    }
    parts.iter().all(|p| {
        p.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
    })
}

pub fn slugify(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect()
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    let truncated: String = s.chars().take(n).collect();
    format!("{truncated}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_name_validation() {
        assert!(valid_full_name("owner/repo"));
        assert!(valid_full_name("coilysiren/repo-recall"));
        assert!(valid_full_name("a.b/c_d"));
        assert!(!valid_full_name(""));
        assert!(!valid_full_name("noslash"));
        assert!(!valid_full_name("a//b"));
        assert!(!valid_full_name("a/b/c"));
        assert!(!valid_full_name("owner/repo;rm -rf"));
        assert!(!valid_full_name("owner/$(whoami)"));
    }
}
