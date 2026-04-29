//! Repo + session + commit + remote-state scan loop.
//!
//! Runs at startup, then on a fixed cadence in a background tokio task, and
//! on-demand when the `recall_refresh` MCP tool is invoked. The MCP host has
//! no live channel into a long-running scan, so progress lands in tracing
//! logs rather than a broadcast.

use std::path::PathBuf;

use chrono::Utc;
use rusqlite::params;

use crate::AppState;
use crate::{commits, db, join, scanner, sessions};

pub async fn run_refresh(state: AppState) -> anyhow::Result<RefreshStats> {
    // Prevent overlapping refreshes. A second invocation while one is in
    // flight returns the no-op marker so callers can distinguish "ran" from
    // "coalesced."
    let _guard = match state.refresh_lock.try_lock() {
        Ok(g) => g,
        Err(_) => {
            tracing::info!("refresh already in progress, skipping");
            return Ok(RefreshStats::coalesced());
        }
    };

    tracing::info!("starting refresh");

    let cwd = state.cwd.clone();
    let db_path = state.db_path.clone();
    let scan_depth = state.scan_depth;
    let commits_per_repo = state.commits_per_repo;

    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<RefreshStats> {
        let conn = db::open(&db_path)?;
        db::wipe(&conn)?;

        // --- repos ---
        let discovered = scanner::scan(&cwd, scan_depth)?;
        let now = Utc::now().timestamp();
        let mut repo_id_by_path: Vec<(i64, PathBuf)> = Vec::with_capacity(discovered.len());
        {
            let tx_ins = conn.unchecked_transaction()?;
            for r in &discovered {
                let remote = commits::remote_info(&r.path);
                tx_ins.execute(
                    "INSERT OR IGNORE INTO repos
                     (path, name, discovered_at, remote_url, default_branch)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        r.path.to_string_lossy(),
                        r.name,
                        now,
                        remote.url,
                        remote.default_branch,
                    ],
                )?;
                let id: i64 = tx_ins.query_row(
                    "SELECT id FROM repos WHERE path = ?1",
                    params![r.path.to_string_lossy()],
                    |row| row.get(0),
                )?;
                repo_id_by_path.push((id, r.path.clone()));
            }
            tx_ins.commit()?;
        }
        let repos_n = repo_id_by_path.len();

        // --- sessions ---
        let Some(projects_dir) = sessions::default_projects_dir() else {
            let commits_n = ingest_commits(&conn, &repo_id_by_path, commits_per_repo)?;
            return Ok(RefreshStats {
                repos: repos_n,
                sessions: 0,
                links: 0,
                commits: commits_n,
                skipped: 0,
                ran: true,
            });
        };
        let files = sessions::list_session_files(&projects_dir)?;

        let mut inserted = 0usize;
        let mut skipped = 0usize;
        let mut links = 0usize;

        let tx_ins = conn.unchecked_transaction()?;
        for path in files.iter() {
            match sessions::parse_session_file(path) {
                Ok(Some(rec)) => {
                    let res = tx_ins.execute(
                        "INSERT OR IGNORE INTO sessions
                         (session_uuid, cwd, started_at, ended_at, message_count, summary,
                          source_file, duration_ms, input_tokens, output_tokens,
                          cache_read_tokens, cache_creation_tokens)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                        params![
                            rec.session_uuid,
                            rec.cwd,
                            rec.started_at,
                            rec.ended_at,
                            rec.message_count,
                            rec.summary,
                            rec.source_file,
                            rec.duration_ms,
                            rec.input_tokens,
                            rec.output_tokens,
                            rec.cache_read_tokens,
                            rec.cache_creation_tokens,
                        ],
                    )?;
                    if res == 0 {
                        skipped += 1;
                        continue;
                    }
                    inserted += 1;
                    let session_id = tx_ins.last_insert_rowid();

                    if let Some(cwd_str) = rec.cwd.as_deref() {
                        if let Some(repo_id) = join::best_repo_for_cwd(cwd_str, &repo_id_by_path) {
                            tx_ins.execute(
                                "INSERT OR IGNORE INTO session_repos (session_id, repo_id, match_type)
                                 VALUES (?1, ?2, 'cwd')",
                                params![session_id, repo_id],
                            )?;
                            links += 1;
                        }
                    }
                }
                Ok(None) => { skipped += 1; }
                Err(e) => {
                    tracing::debug!("parse error {}: {}", path.display(), e);
                    skipped += 1;
                }
            }
        }
        tx_ins.commit()?;

        // --- commits ---
        let commits_n = ingest_commits(&conn, &repo_id_by_path, commits_per_repo)?;

        // --- search index ---
        db::rebuild_search_index(&conn)?;

        Ok(RefreshStats {
            repos: repos_n,
            sessions: inserted,
            links,
            commits: commits_n,
            skipped,
            ran: true,
        })
    })
    .await?;

    let stats = match result {
        Ok(s) => {
            tracing::info!(
                "core refresh done: {} repos, {} sessions, {} links, {} commits ({} skipped)",
                s.repos, s.sessions, s.links, s.commits, s.skipped,
            );
            s
        }
        Err(e) => {
            tracing::error!("refresh failed: {e:?}");
            return Ok(RefreshStats::failed());
        }
    };

    // Remote-state passes. Best-effort. Failures (gh missing, rate-limited,
    // network down) are swallowed at debug! level — they shouldn't break the
    // dashboard.
    let _ci_updated = ingest_ci_status(state.clone()).await;
    let _active_n = ingest_active_repos(state.clone()).await;
    let _content_n = ingest_content_mentions(state.clone()).await;

    *state.last_scan.lock().await = Some(Utc::now());
    state
        .scan_version
        .fetch_add(1, std::sync::atomic::Ordering::Release);

    Ok(stats)
}

async fn ingest_content_mentions(state: AppState) -> usize {
    let db_path = state.db_path.clone();
    tokio::task::spawn_blocking(move || -> anyhow::Result<usize> {
        let conn = db::open(&db_path)?;
        let mut stmt = conn.prepare("SELECT id, name FROM repos")?;
        let needles: Vec<(i64, String)> = stmt
            .query_map([], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })?
            .filter_map(|r| r.ok())
            .collect();

        let mut stmt = conn.prepare("SELECT id, source_file FROM sessions")?;
        let sessions: Vec<(i64, String)> = stmt
            .query_map([], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })?
            .filter_map(|r| r.ok())
            .collect();

        let mut inserted = 0usize;
        let tx_ins = conn.unchecked_transaction()?;
        for (session_id, path) in sessions.iter() {
            let hits = sessions_mod_mentions(std::path::Path::new(path), &needles);
            for repo_id in hits {
                let n = tx_ins.execute(
                    "INSERT OR IGNORE INTO session_repos
                     (session_id, repo_id, match_type)
                     VALUES (?1, ?2, 'content_mention')",
                    rusqlite::params![session_id, repo_id],
                )?;
                inserted += n;
            }
        }
        tx_ins.commit()?;
        Ok(inserted)
    })
    .await
    .ok()
    .and_then(|r| r.ok())
    .unwrap_or(0)
}

fn sessions_mod_mentions(path: &std::path::Path, needles: &[(i64, String)]) -> Vec<i64> {
    crate::sessions::mentions_in_file(path, needles)
}

async fn ingest_ci_status(state: AppState) -> usize {
    let health = tokio::task::spawn_blocking(commits::gh_health)
        .await
        .unwrap_or(commits::GhHealth::Missing);
    *state.gh_health.lock().await = health;
    if health != commits::GhHealth::Ok {
        return 0;
    }
    let my_login = tokio::task::spawn_blocking(commits::my_gh_login)
        .await
        .ok()
        .flatten();
    *state.my_gh_login.lock().await = my_login.clone();
    let my_login = my_login.unwrap_or_default();

    let target_limit = state.remote_target_limit;
    let targets = {
        let db_path = state.db_path.clone();
        match tokio::task::spawn_blocking(
            move || -> anyhow::Result<Vec<(i64, String, String, String)>> {
                let conn = db::open(&db_path)?;
                let sql = if target_limit == 0 {
                    "SELECT r.id, r.remote_url, r.default_branch, r.path
                     FROM repos r
                     LEFT JOIN (
                         SELECT repo_id, MAX(timestamp) AS latest_ts
                         FROM commits GROUP BY repo_id
                     ) c ON c.repo_id = r.id
                     WHERE r.remote_url IS NOT NULL AND r.default_branch IS NOT NULL
                     ORDER BY COALESCE(c.latest_ts, 0) DESC"
                        .to_string()
                } else {
                    format!(
                        "SELECT r.id, r.remote_url, r.default_branch, r.path
                         FROM repos r
                         LEFT JOIN (
                             SELECT repo_id, MAX(timestamp) AS latest_ts
                             FROM commits GROUP BY repo_id
                         ) c ON c.repo_id = r.id
                         WHERE r.remote_url IS NOT NULL AND r.default_branch IS NOT NULL
                         ORDER BY COALESCE(c.latest_ts, 0) DESC
                         LIMIT {target_limit}"
                    )
                };
                let mut stmt = conn.prepare(&sql)?;
                let rows = stmt.query_map([], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                })?;
                let mut out = Vec::new();
                for r in rows {
                    out.push(r?);
                }
                Ok(out)
            },
        )
        .await
        {
            Ok(Ok(v)) => v,
            _ => return 0,
        }
    };

    let jobs: Vec<_> = targets
        .into_iter()
        .filter_map(|(id, url, branch, path)| {
            commits::github_owner_repo(&url).map(|slug| {
                let deploy_wf = commits::find_deploy_workflow(std::path::Path::new(&path));
                (id, slug, branch, deploy_wf)
            })
        })
        .collect();
    let total = jobs.len();
    if total == 0 {
        return 0;
    }

    let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(8));
    let mut set = tokio::task::JoinSet::new();
    for (id, slug, branch, deploy_wf) in jobs {
        let sem = semaphore.clone();
        let login = my_login.clone();
        set.spawn(async move {
            let _permit = sem.acquire_owned().await.ok()?;
            tokio::task::spawn_blocking(move || {
                let ci = commits::ci_status(&slug, &branch);
                let (prs, issues) = match commits::fetch_pr_and_issue_counts(&slug, &login) {
                    Some((p, i)) => (Some(p), Some(i)),
                    None => (None, None),
                };
                let deploy = deploy_wf.as_ref().and_then(|wf| {
                    commits::fetch_deploy_health(&slug, wf, &branch).map(|h| (wf.clone(), h))
                });
                RemoteSnapshot {
                    id,
                    ci,
                    prs,
                    issues,
                    deploy,
                }
            })
            .await
            .ok()
        });
    }

    let mut results: Vec<RemoteSnapshot> = Vec::with_capacity(total);
    while let Some(res) = set.join_next().await {
        if let Ok(Some(snap)) = res {
            results.push(snap);
        }
    }

    let db_path = state.db_path.clone();
    tokio::task::spawn_blocking(move || -> anyhow::Result<usize> {
        let conn = db::open(&db_path)?;
        let tx_ins = conn.unchecked_transaction()?;
        let mut n = 0usize;
        for snap in results {
            let prs = snap.prs.unwrap_or_default();
            let issues_total: Option<i64> = snap.issues.as_ref().map(|i| i.open);
            let issues_assigned: Option<i64> = snap.issues.as_ref().map(|i| i.assigned_to_me);
            let (deploy_wf, deploy_status, deploy_last_success) = match &snap.deploy {
                Some((wf, h)) => (Some(wf.clone()), h.status.clone(), h.last_success_ts),
                None => (None, None, None),
            };
            tx_ins.execute(
                "UPDATE repos
                 SET ci_status = COALESCE(?1, ci_status),
                     open_prs = ?2,
                     draft_prs = ?3,
                     prs_awaiting_my_review = ?4,
                     prs_mine_awaiting_review = ?5,
                     open_issues = COALESCE(?6, open_issues),
                     issues_assigned_to_me = COALESCE(?7, issues_assigned_to_me),
                     deploy_workflow = COALESCE(?8, deploy_workflow),
                     deploy_status = COALESCE(?9, deploy_status),
                     deploy_last_success_ts = COALESCE(?10, deploy_last_success_ts)
                 WHERE id = ?11",
                rusqlite::params![
                    snap.ci,
                    prs.open,
                    prs.draft,
                    prs.awaiting_my_review,
                    prs.mine_awaiting_review,
                    issues_total,
                    issues_assigned,
                    deploy_wf,
                    deploy_status,
                    deploy_last_success,
                    snap.id,
                ],
            )?;
            n += 1;
        }
        tx_ins.commit()?;
        Ok(n)
    })
    .await
    .ok()
    .and_then(|r| r.ok())
    .unwrap_or(0)
}

async fn ingest_active_repos(state: AppState) -> usize {
    if *state.gh_health.lock().await != commits::GhHealth::Ok {
        return 0;
    }
    let actives = tokio::task::spawn_blocking(|| commits::fetch_active_repos(100))
        .await
        .unwrap_or_default();
    if actives.is_empty() {
        return 0;
    }
    let db_path = state.db_path.clone();
    tokio::task::spawn_blocking(move || -> anyhow::Result<usize> {
        let conn = db::open(&db_path)?;
        let rows: Vec<db::ActiveRemoteRepo> = actives
            .into_iter()
            .map(|a| db::ActiveRemoteRepo {
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
        db::upsert_active_remote_repos(&conn, &rows)
    })
    .await
    .ok()
    .and_then(|r| r.ok())
    .unwrap_or(0)
}

struct RemoteSnapshot {
    id: i64,
    ci: Option<String>,
    prs: Option<commits::PrCounts>,
    issues: Option<commits::IssueCounts>,
    deploy: Option<(String, commits::DeployHealth)>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct RefreshStats {
    pub repos: usize,
    pub sessions: usize,
    pub links: usize,
    pub commits: usize,
    pub skipped: usize,
    /// `false` when the call coalesced with an in-flight refresh.
    pub ran: bool,
}

impl RefreshStats {
    fn coalesced() -> Self {
        Self { repos: 0, sessions: 0, links: 0, commits: 0, skipped: 0, ran: false }
    }
    fn failed() -> Self {
        Self { repos: 0, sessions: 0, links: 0, commits: 0, skipped: 0, ran: false }
    }
}

fn ingest_commits(
    conn: &rusqlite::Connection,
    repos: &[(i64, PathBuf)],
    limit_per_repo: usize,
) -> anyhow::Result<usize> {
    let mut total_commits = 0usize;
    let churn_cutoff = chrono::Utc::now().timestamp() - 30 * 86_400;
    let tx_ins = conn.unchecked_transaction()?;
    for (repo_id, repo_path) in repos.iter() {
        match commits::scan(repo_path, limit_per_repo) {
            Ok(records) => {
                for rec in &records {
                    tx_ins.execute(
                        "INSERT OR IGNORE INTO commits
                         (repo_id, sha, author_name, author_email, timestamp, subject)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                        rusqlite::params![
                            repo_id,
                            rec.sha,
                            rec.author_name,
                            rec.author_email,
                            rec.timestamp,
                            rec.subject,
                        ],
                    )?;
                }
                total_commits += records.len();
            }
            Err(e) => {
                tracing::debug!("commits scan failed in {}: {e}", repo_path.display());
            }
        }
        let file_changes = commits::file_changes_since(repo_path, churn_cutoff);
        let churn: i64 = file_changes
            .iter()
            .map(|fc| fc.additions + fc.deletions)
            .sum();
        for fc in &file_changes {
            tx_ins.execute(
                "INSERT INTO file_changes
                 (repo_id, sha, file_path, additions, deletions, author_email, timestamp)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![
                    repo_id,
                    fc.sha,
                    fc.file_path,
                    fc.additions,
                    fc.deletions,
                    fc.author_email,
                    fc.timestamp,
                ],
            )?;
        }
        let snap = commits::worktree_snapshot(repo_path, 50);
        let local = commits::local_state(repo_path);
        tx_ins.execute(
            "UPDATE repos
             SET loc_churn_30d = ?1, untracked_files = ?2, modified_files = ?3,
                 commits_ahead = ?4, commits_behind = ?5, stash_count = ?6,
                 head_ref = ?7, in_progress_op = ?8
             WHERE id = ?9",
            rusqlite::params![
                churn,
                snap.total_untracked,
                snap.total_modified,
                local.commits_ahead,
                local.commits_behind,
                local.stash_count,
                local.head_ref,
                local.in_progress_op,
                repo_id,
            ],
        )?;
        for f in &snap.files {
            tx_ins.execute(
                "INSERT INTO uncommitted_files (repo_id, path, kind) VALUES (?1, ?2, ?3)",
                rusqlite::params![repo_id, f.path, f.kind.as_str()],
            )?;
        }
    }
    tx_ins.commit()?;
    Ok(total_commits)
}
