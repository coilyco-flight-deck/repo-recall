use std::path::PathBuf;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use chrono::Utc;

use crate::db::{ActiveRemoteRepo, CacheDb};
use crate::ingest::claude::sessions_jsonl as sessions;
use crate::ingest::cli_guard::audit_jsonl as audit;
use crate::ingest::git;
use crate::process::join;
use crate::AppState;

pub async fn trigger(State(state): State<AppState>) -> impl IntoResponse {
    tokio::spawn(async move {
        if let Err(e) = run_refresh(state).await {
            tracing::error!("refresh failed: {e:?}");
        }
    });
    (StatusCode::ACCEPTED, "refresh started")
}

/// One ingest source. The fan-out scheduler (#146) gives each its own
/// cadence and watermark; `run_refresh_for` runs an arbitrary subset in a
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Source {
    /// `git log` + churn + worktree/local state. Owns the commit tables.
    GitLog,
    /// `gh` REST: CI status, PR / issue counts, deploy health, the
    /// "clone one" active-repo snapshot. Owns the remote-record tables.
    GithubRemote,
    /// Claude Code session JSONL: sessions, cwd/content/gh-ref joins,
    /// the full-text turn index. Owns the session tables.
    Sessions,
    /// cli-guard audit log under `~/.coily/audit/`. Owns the audit tables.
    CliGuard,
    /// Repo docs (README / AGENTS / FEATURES / file health). No ingest is
    /// wired yet - the slot exists so the scheduler carries its cadence
    Docs,
}

impl Source {
    /// Every source, in sweep order. Discovery runs before all of them.
    pub const ALL: [Source; 5] = [
        Source::GitLog,
        Source::Sessions,
        Source::GithubRemote,
        Source::CliGuard,
        Source::Docs,
    ];

    /// Stable name used for the config key and the watermark table key.
    pub fn name(self) -> &'static str {
        match self {
            Source::GitLog => "git_log",
            Source::GithubRemote => "github_remote",
            Source::Sessions => "sessions",
            Source::CliGuard => "cli_guard",
            Source::Docs => "docs",
        }
    }
}

/// Full-sweep refresh: every source. Used for the initial scan and the
/// manual `POST /api/refresh` trigger.
pub async fn run_refresh(state: AppState) -> anyhow::Result<()> {
    run_refresh_for(state, &Source::ALL).await
}

/// Refresh exactly the sources in `sources`. The fan-out scheduler calls
/// this each tick with the subset whose interval has elapsed (#146).
pub async fn run_refresh_for(state: AppState, sources: &[Source]) -> anyhow::Result<()> {
    // Prevent overlapping refreshes.
    let _guard = match state.refresh_lock.try_lock() {
        Ok(g) => g,
        Err(_) => {
            tracing::debug!("refresh already in progress");
            return Ok(());
        }
    };

    let do_git = sources.contains(&Source::GitLog);
    let do_sessions = sources.contains(&Source::Sessions);
    let do_remote = sources.contains(&Source::GithubRemote);
    let do_cli = sources.contains(&Source::CliGuard);

    tracing::debug!(
        "starting refresh: {}",
        sources
            .iter()
            .map(|s| s.name())
            .collect::<Vec<_>>()
            .join(",")
    );

    let cwd = state.cwd.clone();
    let cache_db = state.cache_db.clone();
    let scan_depth = state.scan_depth;
    let commits_per_repo = state.commits_per_repo;
    let search_index = state.search_index.clone();
    let cutoff_30d = chrono::Utc::now().timestamp() - 30 * 86_400;
    // Turn-index window (#229). Full session turn text is re-parsed and
    // re-indexed every refresh, so the window bounds that work to recent
    let turn_index_days: i64 = std::env::var("REPO_RECALL_TURN_INDEX_DAYS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(30);
    let turn_index_cutoff = if turn_index_days <= 0 {
        0
    } else {
        chrono::Utc::now().timestamp() - turn_index_days * 86_400
    };

    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<RefreshStats> {
        // Discovery: always. The repo table is the join key for every
        // source. `upsert_repo` is idempotent; `prune_repos_not_in` drops
        let discovered = git::discovery::scan(&cwd, scan_depth)?;
        let now = Utc::now().timestamp();
        let repo_id_by_path: Vec<(i64, PathBuf)> = cache_db.write_batch(|w| {
            let mut out = Vec::with_capacity(discovered.len());
            for r in &discovered {
                let remote = git::log::remote_info(&r.path);
                let id = w.upsert_repo(
                    &r.path.to_string_lossy(),
                    &r.name,
                    now,
                    remote.url.as_deref(),
                    remote.default_branch.as_deref(),
                )?;
                out.push((id, r.path.clone()));
            }
            let keep: Vec<i64> = out.iter().map(|(id, _)| *id).collect();
            w.prune_repos_not_in(&keep)?;
            Ok(out)
        })?;
        let mut stats = RefreshStats {
            repos: repo_id_by_path.len(),
            ..RefreshStats::default()
        };

        // sessions source: re-parse the session JSONL, rebuild the session
        // + cwd-join tables. `wipe_sessions` clears only the tables this
        let mut turn_docs: Vec<crate::search::IndexDoc> = Vec::new();
        if do_sessions {
            let projects_dirs = sessions::default_projects_dirs();
            let files = if projects_dirs.is_empty() {
                Vec::new()
            } else {
                sessions::list_session_files(&projects_dirs)?
            };
            let mut inserted = 0usize;
            let mut skipped = 0usize;
            let mut links = 0usize;
            cache_db.write_batch(|w| {
                w.wipe_sessions()?;
                for path in files.iter() {
                    match sessions::parse_session_file(path) {
                        Ok(Some((rec, turns))) => {
                            let (session_id, was_new) = w.upsert_session(&rec)?;
                            if !was_new {
                                skipped += 1;
                                continue;
                            }
                            inserted += 1;
                            // Expand turns into one index doc each, but only
                            // for sessions inside the turn-index window.
                            if rec.started_at.is_none_or(|t| t >= turn_index_cutoff) {
                                turn_docs.extend(session_turn_docs(
                                    session_id,
                                    &rec.session_uuid,
                                    &turns,
                                ));
                            }
                            if let Some(cwd_str) = rec.cwd.as_deref() {
                                if let Some(repo_id) =
                                    join::best_repo_for_cwd(cwd_str, &repo_id_by_path)
                                {
                                    if w.link_session_repo(session_id, repo_id, "cwd")? {
                                        links += 1;
                                    }
                                }
                            }
                        }
                        Ok(None) => skipped += 1,
                        Err(e) => {
                            tracing::debug!("parse error {}: {}", path.display(), e);
                            skipped += 1;
                        }
                    }
                }
                Ok(())
            })?;
            stats.sessions = inserted;
            stats.links = links;
            stats.skipped = skipped;
        }

        // git_log source: commits + churn + worktree/local state. The
        // wipe + rebuild stay in adjacent transactions so the empty window
        if do_git {
            cache_db.write_batch(|w| w.wipe_git_log())?;
            stats.commits = ingest_commits(&cache_db, &repo_id_by_path, commits_per_repo)?;
        }

        // cli_guard source: audit log (#148). Walks every JSONL shard under
        // `~/.coily/audit/` and groups rows by `commit_scope`. Rows whose
        if do_cli {
            cache_db.write_batch(|w| w.wipe_cli_guard())?;
            ingest_cli_guard(&cache_db, &repo_id_by_path)?;
        }

        // Per-repo aggregates the dashboard reads back. Cheap; recompute
        // every sweep off whatever the cache currently holds.
        cache_db.write_batch(|w| w.finalize_repo_aggregates(cutoff_30d))?;

        // Full-text index. Rebuilt only when the sessions source ran: it is
        // the one source that re-parses the turn text the index needs, and
        if do_sessions {
            let mut corpus = cache_db.collect_search_corpus()?;
            corpus.extend(turn_docs);
            if let Err(e) = search_index.rebuild(corpus) {
                tracing::warn!("tantivy rebuild failed (search will serve stale results): {e:?}");
            }
        }

        Ok(stats)
    })
    .await?;

    let stats = match result {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("refresh error: {e}");
            return Ok(());
        }
    };

    // github_remote source: PR + issue counts, deploy status, then the
    // "clone one" active-repo snapshot. Runs as its own async tasks so the
    let remote_updated = if do_remote {
        let n = ingest_remote_state(state.clone()).await;
        let _active = ingest_active_repos(state.clone()).await;
        n
    } else {
        0
    };

    // Content-mention + gh-ref joins belong to the sessions source: both
    // add `session_repos` rows and depend on the session parse above, so
    let content_matches = if do_sessions {
        let n = ingest_content_mentions(state.clone()).await;
        let _gh_refs = ingest_gh_refs(state.clone()).await;
        n
    } else {
        0
    };

    // Advance the watermark for every source this sweep covered. The
    // scheduler measures the next "has my interval elapsed?" check from
    let done_at = Utc::now();
    {
        let cache_db = state.cache_db.clone();
        let names: Vec<&'static str> = sources.iter().map(|s| s.name()).collect();
        let ts = done_at.timestamp();
        let watermark_write = tokio::task::spawn_blocking(move || {
            cache_db.write_batch(|w| {
                for name in names {
                    w.set_refresh_watermark(name, ts)?;
                }
                Ok(())
            })
        })
        .await;
        if let Ok(Err(e)) = watermark_write {
            tracing::warn!("refresh watermark write failed: {e:?}");
        }
    }

    *state.last_scan.lock().await = Some(done_at);
    state
        .scan_version
        .fetch_add(1, std::sync::atomic::Ordering::Release);
    tracing::info!(
        "refresh done: {} repos, {} sessions, {} links, {} commits, {} remote, {} content-matches ({} skipped)",
        stats.repos,
        stats.sessions,
        stats.links,
        stats.commits,
        remote_updated,
        content_matches,
        stats.skipped,
    );
    Ok(())
}

/// Best-effort word-boundary content match: for each session file we've
/// indexed, read it once and add `session_repos` rows with
async fn ingest_content_mentions(state: AppState) -> usize {
    let cache_db = state.cache_db.clone();
    tokio::task::spawn_blocking(move || -> anyhow::Result<usize> {
        let needles = cache_db.iter_repo_ids_and_names()?;
        let sessions = cache_db.iter_session_source_files()?;
        let inserted = cache_db.write_batch(|w| {
            let mut n = 0usize;
            for (session_id, path) in sessions.iter() {
                let hits = sessions::mentions_in_file(std::path::Path::new(path), &needles);
                for repo_id in hits {
                    if w.link_session_repo(*session_id, repo_id, "content_mention")? {
                        n += 1;
                    }
                }
            }
            Ok(n)
        })?;
        Ok(inserted)
    })
    .await
    .ok()
    .and_then(|r| r.ok())
    .unwrap_or(0)
}

/// gh-ref pass. For each session JSONL, scan once for `<owner>/<repo>#<n>`
/// and `github.com/<owner>/<repo>/(pull|issues)/<n>` references; resolve
async fn ingest_gh_refs(state: AppState) -> usize {
    let cache_db = state.cache_db.clone();
    tokio::task::spawn_blocking(move || -> anyhow::Result<usize> {
        let remotes = cache_db.iter_repo_ids_and_remotes()?;
        let mut by_slug: std::collections::HashMap<String, i64> =
            std::collections::HashMap::with_capacity(remotes.len());
        for (id, url) in remotes {
            if let Some(slug) = git::log::github_owner_repo(&url) {
                by_slug.entry(slug.to_ascii_lowercase()).or_insert(id);
            }
        }
        if by_slug.is_empty() {
            return Ok(0);
        }
        let sessions = cache_db.iter_session_source_files()?;
        let inserted = cache_db.write_batch(|w| {
            let mut n = 0usize;
            for (session_id, path) in sessions.iter() {
                let hits = sessions::issue_refs_in_file(std::path::Path::new(path));
                // Track which repo links we've already added per session
                // so the link write-amplification stays at one row per
                let mut linked: std::collections::HashSet<i64> = std::collections::HashSet::new();
                for hit in hits {
                    let slug = format!("{}/{}", hit.owner, hit.repo);
                    if let Some(&repo_id) = by_slug.get(&slug) {
                        if linked.insert(repo_id)
                            && w.link_session_repo(*session_id, repo_id, "gh-ref")?
                        {
                            n += 1;
                        }
                        // Record the issue-level reference. Idempotent on
                        // (repo, issue_number, source_kind, source_id) so
                        w.record_issue_ref(
                            repo_id,
                            hit.issue,
                            crate::db::issue_ref_source::SESSION,
                            *session_id,
                        )?;
                    }
                }
            }
            Ok(n)
        })?;
        Ok(inserted)
    })
    .await
    .ok()
    .and_then(|r| r.ok())
    .unwrap_or(0)
}

/// Parallel GitHub REST fetch across every repo with a GitHub remote +
/// known default branch: open PRs, open issues, deploy workflow health.
async fn ingest_remote_state(state: AppState) -> usize {
    // Skip the entire remote pass while we're inside a rate-limit
    // cooldown window. Set by a prior pass that observed a
    {
        let until_guard = state.remote_backoff_until.lock().await;
        if let Some(until) = *until_guard {
            let now = chrono::Utc::now();
            if now < until {
                let remaining = (until - now).num_seconds().max(0);
                tracing::warn!(
                    "remote-state pass skipped: backoff in effect for {remaining}s more"
                );
                return 0;
            }
        }
    }

    // Re-probe the viewer on every refresh - the user may have logged in
    // since startup, switched accounts, or hit a fresh rate limit. The
    use crate::ingest::github::RemoteFetchState;
    let viewer_state = state.github_client.fetch_user().await;
    let my_login = match &viewer_state {
        RemoteFetchState::Ok(u) => u.login.clone(),
        _ => {
            *state.viewer.lock().await = viewer_state;
            return 0;
        }
    };
    *state.viewer.lock().await = viewer_state;

    let target_limit = state.remote_target_limit;
    let cache_db = state.cache_db.clone();
    let targets = match tokio::task::spawn_blocking(
        move || -> anyhow::Result<Vec<(i64, String, String, String)>> {
            cache_db.remote_targets(target_limit)
        },
    )
    .await
    {
        Ok(Ok(v)) => v,
        _ => return 0,
    };

    // Per-repo dispatch (#91); see docs/forgejo-dispatch.md.
    use crate::ingest::remote_kind::RemoteKind;
    type RemoteJob = (
        i64,
        String,
        String,
        Option<String>,
        std::sync::Arc<dyn crate::ingest::github::GithubClient>,
        &'static str,
    );
    let mut jobs: Vec<RemoteJob> = Vec::new();
    for (id, url, branch, path) in targets {
        let Some((host, slug)) = git::log::remote_host_and_slug(&url) else {
            continue;
        };
        let Some(kind) = state.remote_kind_cache.detect(&host).await else {
            continue;
        };
        let (client, source) = match kind {
            RemoteKind::Github => (
                state.github_client.clone(),
                crate::db::milestone_source::GITHUB,
            ),
            RemoteKind::Forgejo => (
                state.forgejo_client.clone(),
                crate::db::milestone_source::FORGEJO,
            ),
        };
        let deploy_wf = git::log::find_deploy_workflow(std::path::Path::new(&path));
        jobs.push((id, slug, branch, deploy_wf, client, source));
    }
    let total = jobs.len();
    if total == 0 {
        return 0;
    }

    // Bounded concurrency: 8 concurrent fetches is plenty without
    // hammering the rate limit or fork-bombing the laptop.
    let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(8));
    let mut set = tokio::task::JoinSet::new();
    for (id, slug, branch, deploy_wf, client, source) in jobs {
        let sem = semaphore.clone();
        let login = my_login.clone();
        set.spawn(async move {
            let _permit = sem.acquire_owned().await.ok()?;
            // #176, #91: GithubClient trait → octocrab / fixtures / Forgejo.
            let issue_state = client.fetch_open_issues(&slug).await;
            let pr_state = client.fetch_open_prs(&slug).await;
            let milestone_state = client.fetch_open_milestones(&slug).await;
            let deploy_state = match deploy_wf.as_ref() {
                Some(wf) => Some((
                    wf.clone(),
                    client.fetch_deploy_health(&slug, wf, &branch).await,
                )),
                None => None,
            };
            let slug_for_blocking = slug.clone();
            tokio::task::spawn_blocking(move || {
                let slug = slug_for_blocking;
                // `gh`-shell counts are GitHub-only; Forgejo skips them (#91).
                let (prs, issues) = if source == crate::db::milestone_source::GITHUB {
                    match git::log::fetch_pr_and_issue_counts(&slug, &login) {
                        Some((p, i)) => (Some(p), Some(i)),
                        None => (None, None),
                    }
                } else {
                    (None, None)
                };
                use crate::ingest::github::RemoteFetchState;
                let deploy = deploy_state.and_then(|(wf, st)| st.into_option().map(|h| (wf, h)));

                let mut rate_limited = false;
                let mut max_retry_after_secs: Option<u64> = None;
                for st in [
                    pr_state.clone().discard_payload(),
                    issue_state.clone().discard_payload(),
                    milestone_state.clone().discard_payload(),
                ] {
                    if let RemoteFetchState::RateLimited { retry_after_secs } = st {
                        rate_limited = true;
                        if let Some(s) = retry_after_secs {
                            max_retry_after_secs =
                                Some(max_retry_after_secs.map_or(s, |cur| cur.max(s)));
                        }
                    }
                }

                let pr_records = pr_state.into_option().unwrap_or_default();
                let issue_records = issue_state.into_option().unwrap_or_default();
                let milestones = milestone_state.into_option().unwrap_or_default();
                RemoteSnapshot {
                    id,
                    milestone_source: source,
                    prs,
                    issues,
                    deploy,
                    pr_records,
                    issue_records,
                    milestones,
                    rate_limited,
                    max_retry_after_secs,
                }
            })
            .await
            .ok()
        });
    }

    // Collect + write in one sweep. Keeps the cache write window short.
    let mut results: Vec<RemoteSnapshot> = Vec::with_capacity(total);
    while let Some(res) = set.join_next().await {
        if let Ok(Some(snap)) = res {
            results.push(snap);
        }
    }

    // #167: drive the rate-limit backoff state machine off the
    // categorized RemoteFetchState the per-repo tasks recorded above.
    update_remote_backoff(&state, &results).await;

    // #169: substitute the in-memory shadow for any per-repo snapshot
    // that came back rate-limited (or otherwise empty due to a non-Ok
    apply_last_good_shadow(&state, &mut results).await;

    let cache_db = state.cache_db.clone();
    tokio::task::spawn_blocking(move || -> anyhow::Result<usize> {
        cache_db.write_batch(|w| {
            let mut n = 0usize;
            for snap in results {
                let prs = snap.prs.unwrap_or_default();
                let issues_total: Option<i64> = snap.issues.map(|i| i.open);
                let issues_assigned: Option<i64> = snap.issues.map(|i| i.assigned_to_me);
                let (deploy_wf, deploy_status, deploy_last_success) = match snap.deploy {
                    Some((wf, h)) => (Some(wf), h.status, h.last_success_ts),
                    None => (None, None, None),
                };
                w.update_repo_remote_state(
                    snap.id,
                    prs.open,
                    prs.draft,
                    prs.awaiting_my_review,
                    prs.mine_awaiting_review,
                    prs.mine_no_reviewer,
                    prs.my_draft,
                    issues_total,
                    issues_assigned,
                    deploy_wf,
                    deploy_status,
                    deploy_last_success,
                )?;
                for pr in &snap.pr_records {
                    w.upsert_pr_record(snap.id, pr)?;
                }
                for issue in &snap.issue_records {
                    w.upsert_issue_record(snap.id, issue)?;
                }
                for m in &snap.milestones {
                    w.upsert_milestone(snap.id, snap.milestone_source, m)?;
                }
                n += 1;
            }
            Ok(n)
        })
    })
    .await
    .ok()
    .and_then(|r| r.ok())
    .unwrap_or(0)
}

/// Snapshot the viewer's GitHub repos via `gh repo list` and write them into
/// `active_remote_repos`. Skipped silently when `gh` is missing or
async fn ingest_active_repos(state: AppState) -> usize {
    use crate::ingest::github::RemoteFetchState;
    // #91: merge GitHub + Forgejo active-repos; failures degrade independently.
    let gh = match state.github_client.fetch_active_repos(100).await {
        RemoteFetchState::Ok(v) => v,
        _ => Vec::new(),
    };
    let fj = match state.forgejo_client.fetch_active_repos(100).await {
        RemoteFetchState::Ok(v) => v,
        _ => Vec::new(),
    };
    let mut actives = gh;
    actives.extend(fj);
    if actives.is_empty() {
        return 0;
    }
    let cache_db = state.cache_db.clone();
    tokio::task::spawn_blocking(move || -> anyhow::Result<usize> {
        let rows: Vec<ActiveRemoteRepo> = actives
            .into_iter()
            .map(|a| ActiveRemoteRepo {
                id: 0,
                full_name: a.full_name,
                https_url: a.https_url,
                ssh_url: a.ssh_url,
                default_branch: a.default_branch,
                pushed_at: a.pushed_at,
                description: a.description,
                is_fork: a.is_fork,
                is_archived: a.is_archived,
            })
            .collect();
        cache_db.write_batch(|w| w.replace_active_remote_repos(&rows))
    })
    .await
    .ok()
    .and_then(|r| r.ok())
    .unwrap_or(0)
}

/// Walk the just-collected snapshots and reconcile against
/// `AppState.last_good_remote`:
async fn apply_last_good_shadow(state: &AppState, results: &mut [RemoteSnapshot]) {
    let mut shadow = state.last_good_remote.lock().await;
    let now = chrono::Utc::now();
    let mut substituted = 0usize;
    let mut refreshed = 0usize;

    for snap in results.iter_mut() {
        let snapshot_is_blank = snap.prs.is_none()
            && snap.issues.is_none()
            && snap.deploy.is_none()
            && snap.pr_records.is_empty()
            && snap.issue_records.is_empty()
            && snap.milestones.is_empty();

        if snap.rate_limited || snapshot_is_blank {
            if let Some(prior) = shadow.get(&snap.id) {
                snap.prs.clone_from(&prior.prs);
                snap.issues.clone_from(&prior.issues);
                snap.deploy.clone_from(&prior.deploy);
                snap.pr_records.clone_from(&prior.pr_records);
                snap.issue_records.clone_from(&prior.issue_records);
                snap.milestones.clone_from(&prior.milestones);
                substituted += 1;
            }
            continue;
        }

        shadow.insert(
            snap.id,
            crate::CachedRemoteState {
                prs: snap.prs.clone(),
                issues: snap.issues,
                deploy: snap.deploy.clone(),
                pr_records: snap.pr_records.clone(),
                issue_records: snap.issue_records.clone(),
                milestones: snap.milestones.clone(),
                captured_at: now,
            },
        );
        refreshed += 1;
    }

    if substituted > 0 || refreshed > 0 {
        tracing::debug!(
            "last-good shadow: {refreshed} refreshed, {substituted} substituted from prior pass"
        );
    }
}

/// Inspect a finished pass's snapshots and advance or reset the
/// rate-limit backoff stored on `AppState`. Idempotent: a pass with
async fn update_remote_backoff(state: &AppState, results: &[RemoteSnapshot]) {
    let any_rl = results.iter().any(|s| s.rate_limited);
    let max_retry: Option<u64> = results.iter().filter_map(|s| s.max_retry_after_secs).max();

    let mut backoff_secs = state.remote_backoff_secs.lock().await;
    let mut until = state.remote_backoff_until.lock().await;

    if !any_rl {
        if *backoff_secs > 0 || until.is_some() {
            tracing::info!("remote-state pass clean; clearing rate-limit backoff");
        }
        *backoff_secs = 0;
        *until = None;
        return;
    }

    let new_secs = if *backoff_secs == 0 {
        crate::REMOTE_BACKOFF_MIN_SECS
    } else {
        backoff_secs.saturating_mul(2).clamp(
            crate::REMOTE_BACKOFF_MIN_SECS,
            crate::REMOTE_BACKOFF_MAX_SECS,
        )
    };
    *backoff_secs = new_secs;

    let effective = max_retry.unwrap_or(0).max(new_secs);
    let deadline = chrono::Utc::now() + chrono::Duration::seconds(effective as i64);
    *until = Some(deadline);
    tracing::warn!(
        "rate-limit hit on remote pass; next pass blocked for {effective}s (backoff_step={new_secs}s, retry_after={:?})",
        max_retry,
    );
}

struct RemoteSnapshot {
    id: i64,
    /// Milestone `source` tag at persist time (#91).
    milestone_source: &'static str,
    prs: Option<git::log::PrCounts>,
    issues: Option<git::log::IssueCounts>,
    deploy: Option<(String, git::log::DeployHealth)>,
    pr_records: Vec<crate::ingest::github::PrRecordInput>,
    issue_records: Vec<crate::ingest::github::IssueRecordInput>,
    milestones: Vec<crate::ingest::github::MilestoneInput>,
    /// True if any of this snapshot's gh fetchers reported
    /// `RemoteFetchState::RateLimited`. Aggregated across the pass to
    rate_limited: bool,
    /// The largest parsed `Retry-After` (seconds) from any rate-limited
    /// call in this snapshot. The pass-level aggregator keeps the max
    max_retry_after_secs: Option<u64>,
}

/// Expand one session's parsed transcript into `session_turn` index docs
/// (#229): one doc per turn, carrying the joined prompt / model-output /
fn session_turn_docs(
    session_id: i64,
    session_uuid: &str,
    turns: &[crate::ingest::claude::sessions_jsonl::Turn],
) -> Vec<crate::search::IndexDoc> {
    use crate::ingest::claude::sessions_jsonl::TurnRole;
    use crate::process::sanitize::{scrub, SanitizeSource};
    use crate::search::{IndexDoc, TurnPointer};

    let mut docs = Vec::new();
    for (idx, turn) in turns.iter().enumerate() {
        let mut combined = String::new();
        for t in turn.texts.iter().chain(turn.thinking.iter()) {
            if !combined.is_empty() {
                combined.push('\n');
            }
            combined.push_str(t);
        }
        let combined = combined.trim();
        if combined.is_empty() {
            continue;
        }
        let role = match turn.role {
            TurnRole::User => "user",
            TurnRole::Assistant => "assistant",
            TurnRole::System => "system",
        };
        docs.push(IndexDoc {
            kind: "session_turn".into(),
            ref_id: session_id,
            text: scrub(combined, SanitizeSource::SessionText),
            turn: Some(TurnPointer {
                session_uuid: session_uuid.to_string(),
                turn_index: idx as i64,
                turn_role: role.into(),
            }),
        });
    }
    docs
}

#[derive(Default)]
struct RefreshStats {
    repos: usize,
    sessions: usize,
    links: usize,
    commits: usize,
    skipped: usize,
}

/// Walk every JSONL shard under `~/.coily/audit/` (or the path resolved
/// by `audit::default_audit_dir`), parse each row, and insert it keyed
fn ingest_cli_guard(cache_db: &CacheDb, repos: &[(i64, PathBuf)]) -> anyhow::Result<usize> {
    let Some(audit_dir) = audit::default_audit_dir() else {
        return Ok(0);
    };
    let files = audit::list_audit_files(&audit_dir)?;
    if files.is_empty() {
        return Ok(0);
    }
    // Pre-index repos by canonical toplevel path for the per-row lookup.
    let mut by_path: std::collections::HashMap<String, i64> =
        std::collections::HashMap::with_capacity(repos.len());
    for (id, path) in repos {
        by_path.insert(path.to_string_lossy().into_owned(), *id);
    }
    let inserted = cache_db.write_batch(|w| {
        let mut n = 0usize;
        for path in files.iter() {
            match audit::parse_audit_file(path) {
                Ok(records) => {
                    for rec in records {
                        let repo_id = rec
                            .commit_scope
                            .as_deref()
                            .and_then(|s| by_path.get(s).copied())
                            .unwrap_or(0);
                        let (_id, was_new) = w.upsert_audit_event(repo_id, &rec)?;
                        if was_new {
                            n += 1;
                        }
                    }
                }
                Err(e) => tracing::debug!("cli-guard audit parse error {}: {e}", path.display()),
            }
        }
        Ok(n)
    })?;
    Ok(inserted)
}

/// Run `git log` in every discovered repo and bulk-insert the results.
/// Also computes 30-day LOC churn in the same sweep (second git subprocess
fn ingest_commits(
    cache_db: &CacheDb,
    repos: &[(i64, PathBuf)],
    limit_per_repo: usize,
) -> anyhow::Result<usize> {
    let churn_cutoff = chrono::Utc::now().timestamp() - 30 * 86_400;
    cache_db.write_batch(|w| {
        let mut total_commits = 0usize;
        for (repo_id, repo_path) in repos.iter() {
            match git::log::scan(repo_path, limit_per_repo) {
                Ok(records) => {
                    for rec in &records {
                        let (commit_id, _new) = w.upsert_commit(*repo_id, rec)?;
                        // Auto-close trailers (`closes #N`, `fixes #N`, ...)
                        // are repo-implicit: the issue lives in the same
                        for issue_n in crate::process::join::closes_refs_in_text(&rec.subject) {
                            w.record_issue_ref(
                                *repo_id,
                                issue_n,
                                crate::db::issue_ref_source::COMMIT,
                                commit_id,
                            )?;
                        }
                    }
                    total_commits += records.len();
                }
                Err(e) => {
                    tracing::debug!("commits scan failed in {}: {e}", repo_path.display());
                }
            }
            // Per-file change records for the last 30d — source of truth for
            // both the scalar churn total and the hotspot query.
            let file_changes = git::log::file_changes_since(repo_path, churn_cutoff);
            let churn: i64 = file_changes
                .iter()
                .map(|fc| fc.additions + fc.deletions)
                .sum();
            for fc in &file_changes {
                w.insert_file_change(*repo_id, fc)?;
            }
            // Cap per-repo at 50 paths: enough for the dashboard sample,
            // small enough that a pathological refactor cannot blow up the
            let snap = git::log::worktree_snapshot(repo_path, 50);
            let local = git::log::local_state(repo_path);
            let stale_branches =
                git::log::stale_branches(repo_path, crate::process::activity::STALE_BRANCH_SECS);
            w.update_repo_local_state(
                *repo_id,
                churn,
                snap.total_untracked,
                snap.total_modified,
                local.commits_ahead,
                local.commits_behind,
                local.stash_count,
                local.head_ref.as_deref(),
                local.in_progress_op.as_deref(),
                stale_branches,
            )?;
            for f in &snap.files {
                w.insert_uncommitted_file(*repo_id, &f.path, f.kind.as_str())?;
            }
        }
        Ok(total_commits)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest::claude::sessions_jsonl::{Turn, TurnRole};

    fn turn(role: TurnRole, texts: &[&str], thinking: &[&str]) -> Turn {
        Turn {
            role,
            timestamp: None,
            texts: texts.iter().map(|s| s.to_string()).collect(),
            tool_uses: Vec::new(),
            tool_results: Vec::new(),
            thinking: thinking.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// #229: one `session_turn` doc per non-empty turn, with prompt /
    /// output / thinking text scrubbed at ingest.
    #[test]
    fn session_turn_docs_scrub_and_index_each_turn() {
        let turns = vec![
            turn(TurnRole::User, &["redeploy on kai-server please"], &[]),
            turn(
                TurnRole::Assistant,
                &["rotating ghp_AAAABBBBCCCCDDDDEEEEFFFF now"],
                &["the deploy plan looks sound"],
            ),
            turn(TurnRole::User, &[], &[]), // no prose — dropped
        ];
        let docs = session_turn_docs(42, "uuid-1", &turns);

        assert_eq!(docs.len(), 2, "the empty turn is dropped");
        assert!(docs
            .iter()
            .all(|d| d.kind == "session_turn" && d.ref_id == 42));
        assert!(
            !docs[0].text.contains("kai-server"),
            "internal host scrubbed: {}",
            docs[0].text
        );
        assert!(
            !docs[1].text.contains("ghp_AAAA"),
            "token scrubbed: {}",
            docs[1].text
        );
        assert!(
            docs[1].text.contains("the deploy plan looks sound"),
            "thinking step indexed: {}",
            docs[1].text
        );

        let t = docs[1].turn.as_ref().expect("turn pointer");
        assert_eq!(t.turn_index, 1);
        assert_eq!(t.turn_role, "assistant");
        assert_eq!(t.session_uuid, "uuid-1");
    }
}
