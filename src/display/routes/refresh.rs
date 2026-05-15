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

pub async fn run_refresh(state: AppState) -> anyhow::Result<()> {
    // Prevent overlapping refreshes.
    let _guard = match state.refresh_lock.try_lock() {
        Ok(g) => g,
        Err(_) => {
            tracing::debug!("refresh already in progress");
            return Ok(());
        }
    };

    tracing::debug!("starting refresh");

    let cwd = state.cwd.clone();
    let cache_db = state.cache_db.clone();
    let scan_depth = state.scan_depth;
    let commits_per_repo = state.commits_per_repo;
    let search_index = state.search_index.clone();
    let cutoff_30d = chrono::Utc::now().timestamp() - 30 * 86_400;

    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<RefreshStats> {
        // Phase 1: discovery + repo upserts. One write transaction.
        let discovered = git::discovery::scan(&cwd, scan_depth)?;
        let now = Utc::now().timestamp();
        let repo_id_by_path: Vec<(i64, PathBuf)> = cache_db.write_batch(|w| {
            w.wipe()?;
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
            Ok(out)
        })?;
        let repos_n = repo_id_by_path.len();

        // Phase 2: sessions. Same wipe semantics as before — every record
        // in the cache lands inside the same refresh sweep.
        let Some(projects_dir) = sessions::default_projects_dir() else {
            // No Claude projects dir — still try commits before bailing.
            let commits_n = ingest_commits(&cache_db, &repo_id_by_path, commits_per_repo)?;
            cache_db.write_batch(|w| w.finalize_repo_aggregates(cutoff_30d))?;
            return Ok(RefreshStats {
                repos: repos_n,
                sessions: 0,
                links: 0,
                commits: commits_n,
                skipped: 0,
            });
        };
        let files = sessions::list_session_files(&projects_dir)?;

        let mut inserted = 0usize;
        let mut skipped = 0usize;
        let mut links = 0usize;

        cache_db.write_batch(|w| {
            for path in files.iter() {
                match sessions::parse_session_file(path) {
                    Ok(Some(rec)) => {
                        let (session_id, was_new) = w.upsert_session(&rec)?;
                        if !was_new {
                            skipped += 1;
                            continue;
                        }
                        inserted += 1;
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
                    Ok(None) => {
                        skipped += 1;
                    }
                    Err(e) => {
                        tracing::debug!("parse error {}: {}", path.display(), e);
                        skipped += 1;
                    }
                }
            }
            Ok(())
        })?;

        // Phase 3: commits + per-repo state.
        let commits_n = ingest_commits(&cache_db, &repo_id_by_path, commits_per_repo)?;

        // Phase 3.5: docs/repo-dispatch/ files (#92, #113). One write
        // transaction across all repos so a slow filesystem doesn't
        // fragment commits. Best-effort: parse errors per file are
        // logged at `debug!` in the ingest layer.
        ingest_repo_dispatch(&cache_db, &repo_id_by_path)?;

        // Phase 3.6: cli-guard audit log (#148). Walks every JSONL shard
        // under `~/.coily/audit/` and groups rows by `commit_scope`. Rows
        // whose scope didn't match any discovered repo land under
        // `repo_id = 0` so the unrouted bucket stays queryable.
        ingest_cli_guard(&cache_db, &repo_id_by_path)?;

        // Phase 4: precompute aggregates the dashboard reads back.
        cache_db.write_batch(|w| w.finalize_repo_aggregates(cutoff_30d))?;

        // Phase 5: rebuild the tantivy search index.
        let corpus = cache_db.collect_search_corpus()?;
        if let Err(e) = search_index.rebuild(corpus) {
            tracing::warn!("tantivy rebuild failed (search will serve stale results): {e:?}");
        }

        Ok(RefreshStats {
            repos: repos_n,
            sessions: inserted,
            links,
            commits: commits_n,
            skipped,
        })
    })
    .await?;

    let stats =
        match result {
            Ok(s) => {
                tracing::info!(
                "refresh: {} repos, {} sessions, {} links, {} commits ({} skipped). checking CI…",
                s.repos, s.sessions, s.links, s.commits, s.skipped,
            );
                s
            }
            Err(e) => {
                tracing::warn!("refresh error: {e}");
                return Ok(());
            }
        };

    // Second pass: CI/CD status + PR + issue counts. Separate from the main
    // blocking refresh so we can run `gh` subprocesses concurrently (tokio
    // spawn + spawn_blocking) rather than serializing N×network-latency
    // into the scan time. Runs after the main refresh has already surfaced
    // its counts, so the UI updates as soon as the offline data is ready
    // and the remote stuff fills in later.
    let ci_updated = ingest_ci_status(state.clone()).await;

    // 2.5: snapshot the user's "active" GitHub repos (regardless of whether
    // they're cloned into this scan tree). Populates the dashboard's
    // "clone one" panel. Best-effort — `gh` missing / unauthenticated leaves
    // the table empty.
    let _active_repos_n = ingest_active_repos(state.clone()).await;

    // Third pass: content-mention matching. Walks every session JSONL
    // looking for bare-word hits on known repo names. Separate because:
    // (a) it's heavy — N sessions × M repos of string-scanning; (b) it's
    // best-effort, so overcounting is OK and users see a "fuzzy" admission.
    let content_matches = ingest_content_mentions(state.clone()).await;

    // Third-and-a-half pass: gh-ref join. Catches sessions started outside
    // a repo's cwd that nonetheless touched it via `gh` shorthand
    // (`owner/name#42`) or pasted PR/issue URLs. Cheaper than the
    // content-mention scan — single string sweep per file, no per-repo
    // automaton — and the contract is "find a known reference in any text
    // field," so it's robust to JSONL schema changes upstream.
    let gh_ref_matches = ingest_gh_refs(state.clone()).await;
    let _ = gh_ref_matches;

    // Labeled-issue pass (#92, #114). Pulls open structural-ask issues
    // from every GitHub-hosted repo so the recall-dispatch planner can
    // tell which structural questions Kai has already opened. Same
    // best-effort posture as `ingest_ci_status`: failures stay silent.
    //
    // Gated against `state.labeled_ingest_interval_secs` (default 3600s)
    // so the GraphQL secondary-rate-limit budget stays inside what
    // AGENTS.md promises. The outer refresh loop ticks every
    // `interval_secs` (default 150s); without this gate the GraphQL call
    // would fire 24x more often than the documented hourly cadence.
    let should_run_labeled = {
        let mut last = state.last_labeled_ingest.lock().await;
        let now = Utc::now();
        let due = match *last {
            None => true,
            Some(prev) => (now - prev).num_seconds() as u64 >= state.labeled_ingest_interval_secs,
        };
        if due {
            *last = Some(now);
        }
        due
    };
    let _labeled_n = if should_run_labeled {
        ingest_labeled_issues(state.clone()).await
    } else {
        tracing::debug!(
            "labeled-issue ingest gated (next run in <={}s)",
            state.labeled_ingest_interval_secs
        );
        0
    };

    *state.last_scan.lock().await = Some(Utc::now());
    state
        .scan_version
        .fetch_add(1, std::sync::atomic::Ordering::Release);
    tracing::info!(
        "refresh done: {} repos, {} sessions, {} links, {} commits, {} remote, {} content-matches ({} skipped)",
        stats.repos,
        stats.sessions,
        stats.links,
        stats.commits,
        ci_updated,
        content_matches,
        stats.skipped,
    );
    Ok(())
}

/// Best-effort word-boundary content match: for each session file we've
/// indexed, read it once and add `session_repos` rows with
/// `match_type = 'content_mention'` for any repo whose name appears as a
/// bare word. Runs inside a single `spawn_blocking` — IO-heavy rather than
/// CPU-heavy, and serial is fine since a few dozen MB of JSONL parses fast.
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
/// matches against discovered repos by `(owner, repo)` parsed from each
/// repo's GitHub remote URL; write `match_type='gh-ref'` rows. Idempotent
/// per `link_session_repo` (rejects duplicate keys at the redb layer).
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
                // (session, repo).
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
                        // duplicate hits in the same file are absorbed.
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

/// Parallel `gh run list` across every repo with a GitHub remote + known
/// default branch. Returns how many rows we successfully updated. Each
/// subprocess runs in its own `spawn_blocking` so network latency overlaps;
/// a bounded `JoinSet` caps in-flight `gh` calls to avoid fork-bombing.
async fn ingest_ci_status(state: AppState) -> usize {
    // Skip the entire remote pass while we're inside a rate-limit
    // cooldown window. Set by a prior pass that observed a
    // `RateLimited` from any gh fetcher; cleared automatically when
    // the deadline passes.
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

    // Re-probe `gh` on every refresh — the user may have installed it or
    // logged in since startup, and the banner should update.
    let health = tokio::task::spawn_blocking(git::log::gh_health)
        .await
        .unwrap_or(git::log::GhHealth::Missing);
    *state.gh_health.lock().await = health;
    if health != git::log::GhHealth::Ok {
        return 0;
    }
    // Re-probe viewer login so it updates if the user switched accounts.
    let my_login = tokio::task::spawn_blocking(git::log::my_gh_login)
        .await
        .ok()
        .flatten();
    *state.my_gh_login.lock().await = my_login.clone();
    let my_login = my_login.unwrap_or_default();

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

    // Filter to repos we actually know how to query (GitHub-hosted only).
    // Sniff the deploy workflow on disk up front so the gh subprocess block
    // can fan out without re-touching the filesystem.
    let jobs: Vec<_> = targets
        .into_iter()
        .filter_map(|(id, url, branch, path)| {
            git::log::github_owner_repo(&url).map(|slug| {
                let deploy_wf = git::log::find_deploy_workflow(std::path::Path::new(&path));
                (id, slug, branch, deploy_wf)
            })
        })
        .collect();
    let total = jobs.len();
    if total == 0 {
        return 0;
    }

    // Bounded concurrency: 8 concurrent `gh` processes is plenty without
    // hammering the rate limit or fork-bombing the laptop.
    let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(8));
    let mut set = tokio::task::JoinSet::new();
    for (id, slug, branch, deploy_wf) in jobs {
        let sem = semaphore.clone();
        let login = my_login.clone();
        set.spawn(async move {
            let _permit = sem.acquire_owned().await.ok()?;
            tokio::task::spawn_blocking(move || {
                let ci = git::log::ci_status(&slug, &branch);
                let (prs, issues) = match git::log::fetch_pr_and_issue_counts(&slug, &login) {
                    Some((p, i)) => (Some(p), Some(i)),
                    None => (None, None),
                };
                let deploy = deploy_wf.as_ref().and_then(|wf| {
                    git::log::fetch_deploy_health(&slug, wf, &branch).map(|h| (wf.clone(), h))
                });
                // Source 2 + 3 + 4: pull the full individual rows alongside
                // the aggregate counts. Same `gh` call surface — just keep
                // more of the response. Best-effort: a single repo's hiccup
                // leaves these vectors empty rather than breaking the pass.
                use crate::ingest::github::RemoteFetchState;
                let pr_state = crate::ingest::github::fetch_open_prs(&slug);
                let issue_state = crate::ingest::github::fetch_open_issues(&slug);
                let ci_state = crate::ingest::github::fetch_recent_runs(&slug, 20);

                let mut rate_limited = false;
                let mut max_retry_after_secs: Option<u64> = None;
                for st in [
                    pr_state.clone().discard_payload(),
                    issue_state.clone().discard_payload(),
                    ci_state.clone().discard_payload(),
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
                let ci_runs = ci_state.into_option().unwrap_or_default();
                RemoteSnapshot {
                    id,
                    ci,
                    prs,
                    issues,
                    deploy,
                    pr_records,
                    issue_records,
                    ci_runs,
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
                    snap.ci,
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
                for run in &snap.ci_runs {
                    w.upsert_ci_run_record(snap.id, run)?;
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
/// unauthenticated. Caps at 100 repos — enough to surface the user's active
/// workspace, small enough not to balloon the gh API budget.
async fn ingest_active_repos(state: AppState) -> usize {
    if *state.gh_health.lock().await != git::log::GhHealth::Ok {
        return 0;
    }
    let actives = tokio::task::spawn_blocking(|| git::log::fetch_active_repos(100))
        .await
        .unwrap_or_default();
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

/// Inspect a finished pass's snapshots and advance or reset the
/// rate-limit backoff stored on `AppState`. Idempotent: a pass with
/// zero rate-limit hits clears any prior backoff.
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
    ci: Option<String>,
    prs: Option<git::log::PrCounts>,
    issues: Option<git::log::IssueCounts>,
    deploy: Option<(String, git::log::DeployHealth)>,
    pr_records: Vec<crate::ingest::github::PrRecordInput>,
    issue_records: Vec<crate::ingest::github::IssueRecordInput>,
    ci_runs: Vec<crate::ingest::github::CiRunRecordInput>,
    /// True if any of this snapshot's gh fetchers reported
    /// `RemoteFetchState::RateLimited`. Aggregated across the pass to
    /// drive `AppState::remote_backoff_*`.
    rate_limited: bool,
    /// The largest parsed `Retry-After` (seconds) from any rate-limited
    /// call in this snapshot. The pass-level aggregator keeps the max
    /// across all snapshots and seeds the next pass's backoff floor.
    max_retry_after_secs: Option<u64>,
}

struct RefreshStats {
    repos: usize,
    sessions: usize,
    links: usize,
    commits: usize,
    skipped: usize,
}

/// Labels we care about ingesting for the recall-dispatch substrate.
/// `(label, state)` tuples become aliased searches inside a single
/// GraphQL request. Add new entries when new labels join the dispatch
/// convention.
const LABEL_INGEST_TARGETS: &[crate::ingest::github::LabelTarget] = &[
    ("structural-ask", "open"),
    ("autonomous-block", "open"),
    ("repo-dispatch", "open"),
    ("repo-dispatch", "closed"),
];

/// Pull labeled issues for every GitHub-hosted repo on disk via a single
/// GraphQL request (one aliased `search` per target). This is the sole
/// sanctioned GraphQL call site in the codebase — see AGENTS.md "No
/// GraphQL" exception and Source 6 of #155. Best-effort: a missing,
/// unauthenticated, or rate-limited `gh` returns 0 without breaking the
/// refresh.
///
/// Cadence is hourly in steady-state, gated at the call site in
/// `run_refresh` against `AppState.labeled_ingest_interval_secs`
/// (sourced from `refresh.per_source.github_remote_labeled`, default
/// 3600s). The full per-source substrate (#146) is still TODO; this is
/// the surgical mitigation for the labeled-issue site only.
async fn ingest_labeled_issues(state: AppState) -> usize {
    let health = *state.gh_health.lock().await;
    if health != git::log::GhHealth::Ok {
        return 0;
    }
    let cache_db = state.cache_db.clone();
    let slugs = match tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<(i64, String)>> {
        let remotes = cache_db.iter_repo_ids_and_remotes()?;
        Ok(remotes
            .into_iter()
            .filter_map(|(id, url)| git::log::github_owner_repo(&url).map(|s| (id, s)))
            .collect())
    })
    .await
    {
        Ok(Ok(v)) => v,
        _ => return 0,
    };
    if slugs.is_empty() {
        return 0;
    }

    let results = tokio::task::spawn_blocking(move || {
        crate::ingest::github::fetch_labeled_issues_graphql(&slugs, LABEL_INGEST_TARGETS)
    })
    .await
    .unwrap_or_default();

    let cache_db = state.cache_db.clone();
    tokio::task::spawn_blocking(move || -> anyhow::Result<usize> {
        cache_db.write_batch(|w| {
            let mut n = 0usize;
            for (repo_id, label, issues) in results {
                for issue in &issues {
                    w.upsert_labeled_issue(repo_id, &label, issue)?;
                    n += 1;
                }
            }
            Ok(n)
        })
    })
    .await
    .ok()
    .and_then(|r| r.ok())
    .unwrap_or(0)
}

/// Walk every JSONL shard under `~/.coily/audit/` (or the path resolved
/// by `audit::default_audit_dir`), parse each row, and insert it keyed
/// to the repo whose toplevel matches the row's `commit_scope`. Rows
/// from cli-guard's `_unrooted` shard or with no matching repo land
/// under `repo_id = 0` so the unrouted bucket stays queryable.
///
/// No-ops cleanly when the directory is unset or empty. One write
/// transaction for the whole sweep so the audit tables commit atomically.
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

/// Walk each repo's `docs/repo-dispatch/` directory and insert every
/// parsed record into the cache (#92, #113). Bulk-writes inside one
/// transaction so the cache stays self-consistent.
fn ingest_repo_dispatch(cache_db: &CacheDb, repos: &[(i64, PathBuf)]) -> anyhow::Result<()> {
    use crate::ingest::docs::repo_dispatch;
    cache_db.write_batch(|w| {
        for (repo_id, repo_path) in repos.iter() {
            let (records, _errors) = repo_dispatch::dispatches_for_repo(repo_path);
            for rec in &records {
                w.insert_dispatch(*repo_id, rec)?;
            }
        }
        Ok(())
    })
}

/// Run `git log` in every discovered repo and bulk-insert the results.
/// Also computes 30-day LOC churn in the same sweep (second git subprocess
/// per repo) and updates the `repos` row.
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
                        // repo as the commit. Cross-repo references are
                        // emitted as `<owner>/<repo>#N` and handled by the
                        // gh-refs pass below.
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
            // DB.
            let snap = git::log::worktree_snapshot(repo_path, 50);
            let local = git::log::local_state(repo_path);
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
            )?;
            for f in &snap.files {
                w.insert_uncommitted_file(*repo_id, &f.path, f.kind.as_str())?;
            }
        }
        Ok(total_commits)
    })
}
