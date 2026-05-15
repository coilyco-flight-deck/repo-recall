// Cache layer backed by redb. Wipe-on-restart: the file lives under
// `$TMPDIR/repo-recall-<port>/cache.redb` by default and is deleted at
// startup so a fresh process always sees an empty corpus.
//
// Layout: one primary table per entity (id -> JSON-encoded record) plus a
// hand-designed secondary index for every query path so per-repo lookups
// stay sub-linear. Aggregates that the SQL layer used subqueries for
// (`session_count`, `commits_30d`, `authors_30d`) are precomputed at the
// end of refresh and stored on the Repo record itself.
//
// Concurrency: redb gives MVCC, so reads open lightweight read txns
// freely. The single writer is the refresh path (guarded by
// `state.refresh_lock` upstream); request handlers never write to the
// cache. Bulk writes during refresh route through `CacheDb::write_batch`
// so the whole phase commits atomically.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use redb::{
    Database, ReadableDatabase, ReadableTable, ReadableTableMetadata, TableDefinition,
    WriteTransaction,
};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// table definitions
// ---------------------------------------------------------------------------

// Primary tables: id -> JSON record.
const REPOS: TableDefinition<u64, &[u8]> = TableDefinition::new("repos");
const SESSIONS: TableDefinition<u64, &[u8]> = TableDefinition::new("sessions");
const COMMITS: TableDefinition<u64, &[u8]> = TableDefinition::new("commits");
const FILE_CHANGES: TableDefinition<u64, &[u8]> = TableDefinition::new("file_changes");
const UNCOMMITTED_FILES: TableDefinition<u64, &[u8]> = TableDefinition::new("uncommitted_files");
const ACTIVE_REMOTE_REPOS: TableDefinition<u64, &[u8]> =
    TableDefinition::new("active_remote_repos");

// id-allocator counters keyed by entity name ("repo", "session", ...).
const META: TableDefinition<&str, u64> = TableDefinition::new("meta");

// Secondary indexes. Composite keys store the natural sort order so a
// single ranged scan answers the query.
const REPOS_BY_PATH: TableDefinition<&str, u64> = TableDefinition::new("repos_by_path");
const SESSIONS_BY_UUID: TableDefinition<&str, u64> = TableDefinition::new("sessions_by_uuid");
// (started_at, session_id). Sessions without a timestamp use i64::MIN so
// they sort to the bottom under reverse iteration (NULLS LAST DESC).
const SESSIONS_BY_STARTED_AT: TableDefinition<(i64, u64), ()> =
    TableDefinition::new("sessions_by_started_at");
// (session_id, repo_id, match_type) -> (). Match-types per (s,r) are
// stored as separate rows so DISTINCT joins fall out naturally.
const SESSION_REPOS: TableDefinition<(u64, u64, &str), ()> = TableDefinition::new("session_repos");
const SESSION_REPOS_BY_REPO: TableDefinition<(u64, u64, &str), ()> =
    TableDefinition::new("session_repos_by_repo");
// (repo_id, sha) -> commit_id; INSERT OR IGNORE dedup.
const COMMITS_BY_REPO_SHA: TableDefinition<(u64, &str), u64> =
    TableDefinition::new("commits_by_repo_sha");
// (repo_id, timestamp, commit_id) -> author_email. Author lives in the
// value so the per-repo aggregate scan does not need to load each commit.
const COMMITS_BY_REPO_TS: TableDefinition<(u64, i64, u64), &str> =
    TableDefinition::new("commits_by_repo_ts");
// (timestamp, commit_id) -> () for the dashboard's recent-commits scan.
const COMMITS_BY_TS: TableDefinition<(i64, u64), ()> = TableDefinition::new("commits_by_ts");
// (repo_id, timestamp, fc_id) -> () for the per-repo hotspot query.
const FILE_CHANGES_BY_REPO_TS: TableDefinition<(u64, i64, u64), ()> =
    TableDefinition::new("file_changes_by_repo_ts");
const UNCOMMITTED_BY_REPO: TableDefinition<(u64, u64), ()> =
    TableDefinition::new("uncommitted_by_repo");
const ACTIVE_REPOS_BY_FULL_NAME: TableDefinition<&str, u64> =
    TableDefinition::new("active_repos_by_full_name");
const ACTIVE_REPOS_BY_HTTPS_URL: TableDefinition<&str, u64> =
    TableDefinition::new("active_repos_by_https_url");
const ACTIVE_REPOS_BY_PUSHED_AT: TableDefinition<(i64, u64), ()> =
    TableDefinition::new("active_repos_by_pushed_at");

// Labeled issues pulled from GitHub (#92, #114, #115, #116). One row per
// (repo, label, number) so multiple ingest passes can write to the same
// table without colliding. Wipe-on-refresh like everything else.
const LABELED_ISSUES: TableDefinition<u64, &[u8]> = TableDefinition::new("labeled_issues");
// (repo_id, label, number) -> row_id. Primary natural-key dedup.
const LABELED_ISSUES_BY_REPO_LABEL_NUMBER: TableDefinition<(u64, &str, i64), u64> =
    TableDefinition::new("labeled_issues_by_repo_label_number");
// (label, state, repo_id, number) -> () for the cross-repo view ("every
// open structural-ask across the workspace").
const LABELED_ISSUES_BY_LABEL_STATE: TableDefinition<(&str, &str, u64, i64), ()> =
    TableDefinition::new("labeled_issues_by_label_state");

// Dispatch records: parsed `docs/repo-dispatch/*.md` files for each repo.
// Designed in #92, #113 - dispatch artifacts are write-once on disk, with
// status tracked on a thin GitHub issue. The cache mirror lets the per-repo
// view and the MCP surface answer "show me this repo's dispatches" without
// re-walking the filesystem on every request.
const DISPATCHES: TableDefinition<u64, &[u8]> = TableDefinition::new("dispatches");
// (repo_id, -dispatched_at, dispatch_id) -> () — newest-first per repo.
// `dispatched_at` is negated so a forward scan returns newest first; rows
// without a timestamp use i64::MAX so they sort to the bottom.
const DISPATCHES_BY_REPO: TableDefinition<(u64, i64, u64), ()> =
    TableDefinition::new("dispatches_by_repo");

// cli-guard audit events. One row per coily verb invocation. See #148.
const AUDIT_EVENTS: TableDefinition<u64, &[u8]> = TableDefinition::new("audit_events");
// (repo_id, -ts, audit_event_id) -> () — newest-first per repo. Repo `0`
// holds rows whose `commit_scope` didn't match any discovered repo.
const AUDIT_EVENTS_BY_REPO_TS: TableDefinition<(u64, i64, u64), ()> =
    TableDefinition::new("audit_events_by_repo_ts");
// event_id (uuid7 from cli-guard) -> audit_event_id_internal. Dedup key.
const AUDIT_EVENTS_BY_NATURAL_KEY: TableDefinition<&str, u64> =
    TableDefinition::new("audit_events_by_natural_key");

// Issue refs: which sessions/commits touch which issue in which repo.
// Designed in #92 to let recall-dispatch ground per-ticket context in real
// prior work, instead of guessing from in-session reasoning.
const ISSUE_REFS: TableDefinition<u64, &[u8]> = TableDefinition::new("issue_refs");
// (repo_id, issue_number, source_kind, source_id) -> issue_ref_id.
const ISSUE_REFS_BY_REPO_ISSUE: TableDefinition<(u64, u32, &str, u64), u64> =
    TableDefinition::new("issue_refs_by_repo_issue");
// (source_kind, source_id, repo_id, issue_number) -> ().
const ISSUE_REFS_BY_SOURCE: TableDefinition<(&str, u64, u64, u32), ()> =
    TableDefinition::new("issue_refs_by_source");

const META_NEXT_REPO: &str = "next_repo_id";
const META_NEXT_SESSION: &str = "next_session_id";
const META_NEXT_COMMIT: &str = "next_commit_id";
const META_NEXT_FILE_CHANGE: &str = "next_file_change_id";
const META_NEXT_UNCOMMITTED: &str = "next_uncommitted_id";
const META_NEXT_ACTIVE_REPO: &str = "next_active_repo_id";
const META_NEXT_AUDIT_EVENT: &str = "next_audit_event_id";
const META_NEXT_ISSUE_REF: &str = "next_issue_ref_id";
const META_NEXT_DISPATCH: &str = "next_dispatch_id";
const META_NEXT_LABELED_ISSUE: &str = "next_labeled_issue_id";

// ---------------------------------------------------------------------------
// public API surface (unchanged from the SQLite version)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct FileHotspot {
    pub file_path: String,
    pub churn: i64,
    pub commits: i64,
    pub authors: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchHit {
    pub kind: String,
    pub ref_id: i64,
    pub text: String,
    pub extra: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Repo {
    pub id: i64,
    pub path: String,
    pub name: String,
    pub session_count: i64,
    pub commits_30d: i64,
    pub loc_churn_30d: i64,
    pub untracked_files: i64,
    pub modified_files: i64,
    pub authors_30d: i64,
    pub ci_status: Option<String>,
    pub commits_ahead: i64,
    pub commits_behind: i64,
    pub stash_count: i64,
    pub head_ref: Option<String>,
    pub in_progress_op: Option<String>,
    pub open_prs: i64,
    pub draft_prs: i64,
    pub open_issues: i64,
    pub prs_awaiting_my_review: i64,
    pub prs_mine_awaiting_review: i64,
    pub prs_mine_no_reviewer: i64,
    pub my_draft_prs: i64,
    pub issues_assigned_to_me: i64,
    pub deploy_workflow: Option<String>,
    pub deploy_status: Option<String>,
    pub deploy_last_success_ts: Option<i64>,
    pub remote_url: Option<String>,
    pub default_branch: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: i64,
    pub session_uuid: String,
    pub cwd: Option<String>,
    pub started_at: Option<i64>,
    pub ended_at: Option<i64>,
    pub message_count: i64,
    pub user_message_count: i64,
    pub assistant_message_count: i64,
    /// Most-recent prompt the user sent in this session, sourced from the
    /// `last-prompt` JSONL line type. Replaces the prior `summary` field
    /// (which used the first user message — less useful for "what was this
    /// session doing").
    pub last_prompt: Option<String>,
    pub source_file: String,
    pub duration_ms: Option<i64>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_creation_tokens: i64,
    pub parent_uuid: Option<String>,
    pub request_id: Option<String>,
    pub message_id: Option<String>,
    pub is_sidechain_count: i64,
    pub models_used: Vec<String>,
    pub tools_used: Vec<String>,
    /// `{ "<tool>": { "calls": N, "errors": N } }`.
    pub tool_call_counts_json: String,
    /// `{ "<stop_reason>": N }`.
    pub stop_reason_counts_json: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionWithRepos {
    pub session: Session,
    pub repos: Vec<(i64, String, String)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Commit {
    pub id: i64,
    pub repo_id: i64,
    pub sha: String,
    pub author_name: String,
    pub author_email: String,
    pub timestamp: i64,
    pub subject: String,
}

/// One row from coily's audit log, joined to the repo via `commit_scope`.
/// `repo_id == 0` means the row's scope didn't match any discovered repo
/// (the cli-guard `_unrooted` shard or a workspace outside `cwd`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEvent {
    pub id: i64,
    pub repo_id: i64,
    pub event_id: String,
    pub ts: i64,
    pub decision: String,
    pub verb: String,
    pub argv: Vec<String>,
    pub exit_code: Option<i64>,
    pub duration_ms: Option<i64>,
    pub commit_scope: Option<String>,
    pub audit_override: bool,
    pub source_file: String,
    pub session_id: Option<String>,
    pub version: Option<String>,
    pub error: Option<String>,
    pub stderr_tail: Option<String>,
    pub repo_root: Option<String>,
    pub cwd_subprocess: Option<String>,
    pub cwd_at_invocation: Option<String>,
    pub egress: Vec<crate::ingest::cli_guard::audit_jsonl::EgressEntry>,
    pub profile_decision: Option<crate::ingest::cli_guard::audit_jsonl::ProfileDecision>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CommitWithRepo {
    pub commit: Commit,
    pub repo_id: i64,
    pub repo_name: String,
    pub repo_path: String,
    pub repo_remote_url: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UncommittedGroup {
    pub repo_id: i64,
    pub repo_name: String,
    pub repo_path: String,
    pub repo_remote_url: Option<String>,
    pub total: i64,
    pub sample: Vec<(String, String)>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CiFailure {
    pub repo_id: i64,
    pub repo_name: String,
    pub repo_path: String,
    pub remote_url: Option<String>,
    pub default_branch: Option<String>,
}

/// A reference from a session or commit to a GitHub issue or PR in a
/// known repo. Populated during ingest from extractors in
/// `process::join` (`gh_refs_with_issue_in_text`, `closes_refs_in_text`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueRef {
    pub id: i64,
    pub repo_id: i64,
    pub issue_number: u32,
    /// `"session"` or `"commit"` — kept as a string to leave room for
    /// future sources (PR review comments, dispatch files) without
    /// breaking the redb schema.
    pub source_kind: String,
    pub source_id: i64,
}

/// Source-kind sentinels for `record_issue_ref`. Defined here so callers
/// can't typo the string at the call site.
pub mod issue_ref_source {
    pub const SESSION: &str = "session";
    pub const COMMIT: &str = "commit";
}

/// One labeled GitHub issue row in the cache. Records a single
/// `(repo, label, issue)` association — the same underlying issue
/// can land in multiple rows (one per matching label of interest),
/// which keeps downstream queries on a single index.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LabeledIssueRow {
    pub id: i64,
    pub repo_id: i64,
    /// Lowercase label name (kept distinct from the `labels` field which
    /// preserves GitHub's casing for display).
    pub label: String,
    pub number: i64,
    pub title: String,
    pub created_at: i64,
    pub closed_at: Option<i64>,
    /// `"OPEN"` / `"CLOSED"`.
    pub state: String,
    pub labels: Vec<String>,
}

/// One stored dispatch row in the cache. Mirrors the parsed
/// `docs/repo-dispatch/*.md` shape with the `repo_id` and assigned
/// `dispatch_id` added.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DispatchRow {
    pub id: i64,
    pub repo_id: i64,
    pub file_path: String,
    pub slug: String,
    pub issue_refs: Vec<(String, String, u32)>,
    pub score: Option<i64>,
    pub autonomy_confidence: Option<i64>,
    pub autonomy_confidence_basis: Option<String>,
    pub prompt_hash: Option<String>,
    pub dispatched_at: Option<i64>,
    pub tracking_issue: Option<(String, String, u32)>,
}

/// Aggregate of dispatch outcomes for one repo (or rolled up across
/// the workspace). Computed on demand by `autonomy_metrics` (#92, #116).
#[derive(Debug, Clone, Default, Serialize)]
pub struct DispatchBucket {
    pub successes: i64,
    pub abandons: i64,
    pub blocks: i64,
    pub open: i64,
    pub total: i64,
}

impl DispatchBucket {
    pub fn success_rate(&self) -> f64 {
        let closed = self.successes + self.abandons + self.blocks;
        if closed == 0 {
            0.0
        } else {
            self.successes as f64 / closed as f64
        }
    }
}

/// Per-repo aggregate row.
#[derive(Debug, Clone, Serialize)]
pub struct RepoDispatchMetrics {
    pub repo_id: i64,
    pub repo_name: String,
    pub bucket: DispatchBucket,
    pub success_rate: f64,
}

/// Top-level rollup returned by `autonomy_metrics`.
#[derive(Debug, Clone, Serialize)]
pub struct AutonomyMetrics {
    pub overall: DispatchBucket,
    pub overall_success_rate: f64,
    pub per_repo: Vec<RepoDispatchMetrics>,
}

/// One dispatch-substrate signal (#92, #115). Distinct from the
/// per-Repo `derive_action_signals` because the input rows live in
/// `LABELED_ISSUES`, not on the `Repo` row itself.
#[derive(Debug, Clone, Serialize)]
pub struct DispatchSignal {
    pub repo_id: i64,
    pub signal: &'static str,
    pub detail: String,
}

/// Bundle returned by `ticket_history`: the sessions and commits that
/// reference a given issue in a given repo. Consumers (recall-dispatch,
/// JSON API) decide how to render.
#[derive(Debug, Clone, Serialize)]
pub struct TicketHistory {
    pub repo_id: i64,
    pub issue_number: u32,
    pub sessions: Vec<Session>,
    pub commits: Vec<Commit>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActiveRemoteRepo {
    pub id: i64,
    pub full_name: String,
    pub https_url: String,
    pub ssh_url: Option<String>,
    pub default_branch: Option<String>,
    pub pushed_at: Option<i64>,
    pub description: Option<String>,
    pub is_fork: bool,
    pub is_archived: bool,
}

// ---------------------------------------------------------------------------
// records persisted to disk (extra fields beyond the public types)
// ---------------------------------------------------------------------------

/// Internal on-disk record for `file_changes` rows. The dashboard never
/// reads these directly; they exist for the hotspot aggregation.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct FileChangeRecord {
    id: i64,
    repo_id: i64,
    sha: String,
    file_path: String,
    additions: i64,
    deletions: i64,
    author_email: String,
    timestamp: i64,
}

/// Internal on-disk record for `uncommitted_files` rows.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct UncommittedFileRecord {
    id: i64,
    repo_id: i64,
    path: String,
    kind: String,
}

// ---------------------------------------------------------------------------
// CacheDb
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct CacheDb {
    db: Arc<Database>,
}

impl std::fmt::Debug for CacheDb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CacheDb").finish_non_exhaustive()
    }
}

impl CacheDb {
    /// Open the cache file at `<dir>/cache.redb`, deleting any prior file
    /// first. Wipe-on-restart matches the SQLite-era contract.
    pub fn open_in_dir(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir).with_context(|| format!("create cache dir: {dir:?}"))?;
        let path = dir.join("cache.redb");
        let _ = std::fs::remove_file(&path);
        Self::open_at(path)
    }

    pub fn open_at(path: PathBuf) -> Result<Self> {
        let db = Database::create(&path).with_context(|| format!("open cache redb at {path:?}"))?;
        // Pre-create every table so first reads do not error on a fresh file.
        let write = db.begin_write()?;
        {
            let _ = write.open_table(REPOS)?;
            let _ = write.open_table(SESSIONS)?;
            let _ = write.open_table(COMMITS)?;
            let _ = write.open_table(FILE_CHANGES)?;
            let _ = write.open_table(UNCOMMITTED_FILES)?;
            let _ = write.open_table(ACTIVE_REMOTE_REPOS)?;
            let _ = write.open_table(META)?;
            let _ = write.open_table(REPOS_BY_PATH)?;
            let _ = write.open_table(SESSIONS_BY_UUID)?;
            let _ = write.open_table(SESSIONS_BY_STARTED_AT)?;
            let _ = write.open_table(SESSION_REPOS)?;
            let _ = write.open_table(SESSION_REPOS_BY_REPO)?;
            let _ = write.open_table(COMMITS_BY_REPO_SHA)?;
            let _ = write.open_table(COMMITS_BY_REPO_TS)?;
            let _ = write.open_table(COMMITS_BY_TS)?;
            let _ = write.open_table(FILE_CHANGES_BY_REPO_TS)?;
            let _ = write.open_table(UNCOMMITTED_BY_REPO)?;
            let _ = write.open_table(ACTIVE_REPOS_BY_FULL_NAME)?;
            let _ = write.open_table(ACTIVE_REPOS_BY_HTTPS_URL)?;
            let _ = write.open_table(ACTIVE_REPOS_BY_PUSHED_AT)?;
            let _ = write.open_table(AUDIT_EVENTS)?;
            let _ = write.open_table(AUDIT_EVENTS_BY_REPO_TS)?;
            let _ = write.open_table(AUDIT_EVENTS_BY_NATURAL_KEY)?;
            let _ = write.open_table(ISSUE_REFS)?;
            let _ = write.open_table(ISSUE_REFS_BY_REPO_ISSUE)?;
            let _ = write.open_table(ISSUE_REFS_BY_SOURCE)?;
            let _ = write.open_table(DISPATCHES)?;
            let _ = write.open_table(DISPATCHES_BY_REPO)?;
            let _ = write.open_table(LABELED_ISSUES)?;
            let _ = write.open_table(LABELED_ISSUES_BY_REPO_LABEL_NUMBER)?;
            let _ = write.open_table(LABELED_ISSUES_BY_LABEL_STATE)?;
        }
        write.commit()?;
        Ok(Self { db: Arc::new(db) })
    }

    /// Run a closure inside a single write transaction. All mutations
    /// commit atomically when the closure returns Ok.
    pub fn write_batch<F, R>(&self, f: F) -> Result<R>
    where
        F: FnOnce(&CacheWriter) -> Result<R>,
    {
        let txn = self.db.begin_write()?;
        let res = {
            let writer = CacheWriter { txn: &txn };
            f(&writer)?
        };
        txn.commit()?;
        Ok(res)
    }

    /// Truncate every cache table. Called at the start of every refresh.
    pub fn wipe(&self) -> Result<()> {
        self.write_batch(|w| w.wipe())
    }

    // -----------------------------------------------------------------
    // reads
    // -----------------------------------------------------------------

    /// Newest-first scan of audit events for one repo. `repo_id == 0`
    /// returns rows from cli-guard's `_unrooted` shard. `since_ts` is the
    /// inclusive lower bound (unix seconds); `None` means no lower bound.
    /// `limit` caps the result count.
    pub fn audit_events_for_repo(
        &self,
        repo_id: i64,
        since_ts: Option<i64>,
        limit: usize,
    ) -> Result<Vec<AuditEvent>> {
        let read = self.db.begin_read()?;
        let idx = read.open_table(AUDIT_EVENTS_BY_REPO_TS)?;
        let evts = read.open_table(AUDIT_EVENTS)?;
        let key = id_to_u64(repo_id);
        let lo = (key, i64::MIN, 0u64);
        let hi = (key, i64::MAX, u64::MAX);
        let mut out = Vec::with_capacity(limit.min(64));
        for row in idx.range(lo..=hi)? {
            let (k, _v) = row?;
            let (_repo, neg_ts, id) = k.value();
            let ts = -neg_ts;
            if let Some(since) = since_ts {
                if ts < since {
                    break;
                }
            }
            if let Some(blob) = evts.get(id)? {
                let e: AuditEvent = serde_json::from_slice(blob.value())?;
                out.push(e);
                if out.len() >= limit {
                    break;
                }
            }
        }
        Ok(out)
    }

    /// Iterate every ingested audit event. Test helper - prefer the
    /// scoped read above in production code paths.
    pub fn list_all_audit_events(&self) -> Result<Vec<AuditEvent>> {
        let read = self.db.begin_read()?;
        let t = read.open_table(AUDIT_EVENTS)?;
        let mut out = Vec::new();
        for row in t.iter()? {
            let (_k, v) = row?;
            let e: AuditEvent = serde_json::from_slice(v.value())?;
            out.push(e);
        }
        Ok(out)
    }

    pub fn list_repos_with_counts(&self) -> Result<Vec<Repo>> {
        let read = self.db.begin_read()?;
        let t = read.open_table(REPOS)?;
        let mut out: Vec<Repo> = Vec::new();
        for row in t.iter()? {
            let (_k, v) = row?;
            let r: Repo = serde_json::from_slice(v.value())?;
            out.push(r);
        }
        Ok(out)
    }

    pub fn get_repo(&self, id: i64) -> Result<Option<Repo>> {
        let read = self.db.begin_read()?;
        let t = read.open_table(REPOS)?;
        let Some(g) = t.get(id_to_u64(id))? else {
            return Ok(None);
        };
        let r: Repo = serde_json::from_slice(g.value())?;
        Ok(Some(r))
    }

    pub fn sessions_for_repo(&self, repo_id: i64) -> Result<Vec<Session>> {
        let read = self.db.begin_read()?;
        let by_repo = read.open_table(SESSION_REPOS_BY_REPO)?;
        let sessions_t = read.open_table(SESSIONS)?;
        let key_lo = (id_to_u64(repo_id), 0u64, "");
        let key_hi = (id_to_u64(repo_id), u64::MAX, "\u{10ffff}");
        let mut session_ids: BTreeSet<u64> = BTreeSet::new();
        for row in by_repo.range(key_lo..=key_hi)? {
            let (k, _) = row?;
            let (_, sid, _) = k.value();
            session_ids.insert(sid);
        }
        let mut out: Vec<Session> = Vec::with_capacity(session_ids.len());
        for sid in session_ids {
            if let Some(g) = sessions_t.get(sid)? {
                let s: Session = serde_json::from_slice(g.value())?;
                out.push(s);
            }
        }
        // ORDER BY started_at DESC NULLS LAST.
        out.sort_by(|a, b| order_by_started_at_desc(a.started_at, b.started_at));
        Ok(out)
    }

    pub fn recent_sessions(&self, limit: i64) -> Result<Vec<SessionWithRepos>> {
        let read = self.db.begin_read()?;
        let by_ts = read.open_table(SESSIONS_BY_STARTED_AT)?;
        let sessions_t = read.open_table(SESSIONS)?;
        let mut out: Vec<Session> = Vec::with_capacity(limit.max(0) as usize);
        for row in by_ts.iter()?.rev() {
            let (k, _) = row?;
            let (_ts, sid) = k.value();
            let Some(g) = sessions_t.get(sid)? else {
                continue;
            };
            let s: Session = serde_json::from_slice(g.value())?;
            out.push(s);
            if out.len() as i64 >= limit {
                break;
            }
        }
        let mut with_repos = Vec::with_capacity(out.len());
        for s in out {
            let repos = self.repos_for_session(s.id)?;
            with_repos.push(SessionWithRepos { session: s, repos });
        }
        Ok(with_repos)
    }

    pub fn get_session(&self, id: i64) -> Result<Option<SessionWithRepos>> {
        let session = {
            let read = self.db.begin_read()?;
            let t = read.open_table(SESSIONS)?;
            match t.get(id_to_u64(id))? {
                Some(g) => Some(serde_json::from_slice::<Session>(g.value())?),
                None => None,
            }
        };
        let Some(s) = session else {
            return Ok(None);
        };
        let repos = self.repos_for_session(s.id)?;
        Ok(Some(SessionWithRepos { session: s, repos }))
    }

    pub fn repos_for_session(&self, session_id: i64) -> Result<Vec<(i64, String, String)>> {
        let read = self.db.begin_read()?;
        let session_repos = read.open_table(SESSION_REPOS)?;
        let repos_t = read.open_table(REPOS)?;
        let key_lo = (id_to_u64(session_id), 0u64, "");
        let key_hi = (id_to_u64(session_id), u64::MAX, "\u{10ffff}");
        let mut seen: BTreeSet<u64> = BTreeSet::new();
        for row in session_repos.range(key_lo..=key_hi)? {
            let (k, _) = row?;
            let (_sid, rid, _mt) = k.value();
            seen.insert(rid);
        }
        let mut rows: Vec<(i64, String, String)> = Vec::with_capacity(seen.len());
        for rid in seen {
            if let Some(g) = repos_t.get(rid)? {
                let r: Repo = serde_json::from_slice(g.value())?;
                rows.push((r.id, r.name, r.path));
            }
        }
        // ORDER BY r.name COLLATE NOCASE ASC.
        rows.sort_by_key(|r| r.1.to_lowercase());
        Ok(rows)
    }

    /// Returns `(cwd_matches, content_matches)`.
    #[allow(clippy::type_complexity)]
    pub fn repos_for_session_by_match(
        &self,
        session_id: i64,
    ) -> Result<(Vec<(i64, String, String)>, Vec<(i64, String, String)>)> {
        let read = self.db.begin_read()?;
        let session_repos = read.open_table(SESSION_REPOS)?;
        let repos_t = read.open_table(REPOS)?;
        let key_lo = (id_to_u64(session_id), 0u64, "");
        let key_hi = (id_to_u64(session_id), u64::MAX, "\u{10ffff}");
        let mut by_match: HashMap<String, Vec<(i64, String, String)>> = HashMap::new();
        for row in session_repos.range(key_lo..=key_hi)? {
            let (k, _) = row?;
            let (_sid, rid, mt) = k.value();
            if let Some(g) = repos_t.get(rid)? {
                let r: Repo = serde_json::from_slice(g.value())?;
                by_match
                    .entry(mt.to_string())
                    .or_default()
                    .push((r.id, r.name, r.path));
            }
        }
        for v in by_match.values_mut() {
            v.sort_by_key(|r| r.1.to_lowercase());
        }
        let cwd = by_match.remove("cwd").unwrap_or_default();
        let gh_ref = by_match.remove("gh-ref").unwrap_or_default();
        let content = by_match.remove("content_mention").unwrap_or_default();
        // Anything else (future match types) defaults into the cwd bucket
        // for the same reason the SQLite version's match arm did: callers
        // treat non-content matches as the primary list. `gh-ref` is named
        // explicitly so it widens cwd without tripping the unknown-type
        // debug log.
        let mut cwd_combined = cwd;
        for r in gh_ref {
            if !cwd_combined.iter().any(|x| x.0 == r.0) {
                cwd_combined.push(r);
            }
        }
        for (mt, mut v) in by_match {
            tracing::debug!("session {session_id}: unknown match_type {mt:?}");
            cwd_combined.append(&mut v);
        }
        Ok((cwd_combined, content))
    }

    pub fn earliest_session_ts(&self) -> Result<Option<i64>> {
        let read = self.db.begin_read()?;
        let by_ts = read.open_table(SESSIONS_BY_STARTED_AT)?;
        // Sessions with NULL started_at use i64::MIN; skip those.
        for row in by_ts.iter()? {
            let (k, _) = row?;
            let (ts, _) = k.value();
            if ts == i64::MIN {
                continue;
            }
            return Ok(Some(ts));
        }
        Ok(None)
    }

    pub fn uncommitted_by_repo(
        &self,
        max_repos: i64,
        files_per_repo: usize,
    ) -> Result<Vec<UncommittedGroup>> {
        let read = self.db.begin_read()?;
        let repos_t = read.open_table(REPOS)?;
        let by_repo = read.open_table(UNCOMMITTED_BY_REPO)?;
        let files_t = read.open_table(UNCOMMITTED_FILES)?;

        // Collect groups keyed by repo_id with their dirty totals + files.
        struct Acc {
            repo: Repo,
            total: i64,
            samples: Vec<(String, String)>,
        }
        let mut acc: HashMap<u64, Acc> = HashMap::new();
        for row in by_repo.iter()? {
            let (k, _) = row?;
            let (rid, fid) = k.value();
            let entry = match acc.entry(rid) {
                std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
                std::collections::hash_map::Entry::Vacant(slot) => {
                    let Some(g) = repos_t.get(rid)? else {
                        continue;
                    };
                    let repo: Repo = serde_json::from_slice(g.value())?;
                    let total = repo.untracked_files + repo.modified_files;
                    if total <= 0 {
                        continue;
                    }
                    slot.insert(Acc {
                        repo,
                        total,
                        samples: Vec::new(),
                    })
                }
            };
            if let Some(g) = files_t.get(fid)? {
                let f: UncommittedFileRecord = serde_json::from_slice(g.value())?;
                entry.samples.push((f.path, f.kind));
            }
        }

        let mut groups: Vec<UncommittedGroup> = acc
            .into_values()
            .map(|a| {
                // Modified rows first (descending kind), then untracked,
                // each sub-group sorted by path. Mirrors the SQLite ORDER.
                let mut samples = a.samples;
                samples.sort_by(|x, y| {
                    y.1.cmp(&x.1) // kind DESC: 'untracked' < 'modified', want modified first
                        .then_with(|| x.0.cmp(&y.0))
                });
                UncommittedGroup {
                    repo_id: a.repo.id,
                    repo_name: a.repo.name.clone(),
                    repo_path: a.repo.path.clone(),
                    repo_remote_url: a.repo.remote_url.clone(),
                    total: a.total,
                    sample: samples.into_iter().take(files_per_repo).collect(),
                }
            })
            .collect();
        // ORDER BY total DESC, name ASC NOCASE.
        groups.sort_by(|a, b| {
            b.total
                .cmp(&a.total)
                .then_with(|| a.repo_name.to_lowercase().cmp(&b.repo_name.to_lowercase()))
        });
        groups.truncate(max_repos.max(0) as usize);
        Ok(groups)
    }

    pub fn failing_ci_repos(&self) -> Result<Vec<CiFailure>> {
        let mut out: Vec<CiFailure> = Vec::new();
        for r in self.list_repos_with_counts()? {
            if r.ci_status.as_deref() == Some("failure") {
                out.push(CiFailure {
                    repo_id: r.id,
                    repo_name: r.name,
                    repo_path: r.path,
                    remote_url: r.remote_url,
                    default_branch: r.default_branch,
                });
            }
        }
        out.sort_by_key(|r| r.repo_name.to_lowercase());
        Ok(out)
    }

    pub fn counts(&self) -> Result<(i64, i64, i64, i64)> {
        let read = self.db.begin_read()?;
        let repos = read.open_table(REPOS)?.len()? as i64;
        let sessions = read.open_table(SESSIONS)?.len()? as i64;
        let links = read.open_table(SESSION_REPOS)?.len()? as i64;
        let commits = read.open_table(COMMITS)?.len()? as i64;
        Ok((repos, sessions, links, commits))
    }

    pub fn recent_commits(
        &self,
        limit: i64,
        author_filter: Option<&str>,
    ) -> Result<Vec<CommitWithRepo>> {
        let read = self.db.begin_read()?;
        let by_ts = read.open_table(COMMITS_BY_TS)?;
        let commits_t = read.open_table(COMMITS)?;
        let repos_t = read.open_table(REPOS)?;
        let mut out: Vec<CommitWithRepo> = Vec::new();
        for row in by_ts.iter()?.rev() {
            let (k, _) = row?;
            let (_ts, cid) = k.value();
            let Some(g) = commits_t.get(cid)? else {
                continue;
            };
            let c: Commit = serde_json::from_slice(g.value())?;
            if let Some(email) = author_filter {
                if c.author_email != email {
                    continue;
                }
            }
            let (rname, rpath, rurl) = match repos_t.get(id_to_u64(c.repo_id))? {
                Some(g) => {
                    let r: Repo = serde_json::from_slice(g.value())?;
                    (r.name, r.path, r.remote_url)
                }
                None => (String::new(), String::new(), None),
            };
            out.push(CommitWithRepo {
                repo_id: c.repo_id,
                repo_name: rname,
                repo_path: rpath,
                repo_remote_url: rurl,
                commit: c,
            });
            if out.len() as i64 >= limit {
                break;
            }
        }
        Ok(out)
    }

    pub fn commits_for_repo(&self, repo_id: i64, limit: i64) -> Result<Vec<Commit>> {
        let read = self.db.begin_read()?;
        let by_repo_ts = read.open_table(COMMITS_BY_REPO_TS)?;
        let commits_t = read.open_table(COMMITS)?;
        let key_lo = (id_to_u64(repo_id), i64::MIN, 0u64);
        let key_hi = (id_to_u64(repo_id), i64::MAX, u64::MAX);
        let mut out: Vec<Commit> = Vec::new();
        for row in by_repo_ts.range(key_lo..=key_hi)?.rev() {
            let (k, _) = row?;
            let (_rid, _ts, cid) = k.value();
            let Some(g) = commits_t.get(cid)? else {
                continue;
            };
            let c: Commit = serde_json::from_slice(g.value())?;
            out.push(c);
            if out.len() as i64 >= limit {
                break;
            }
        }
        Ok(out)
    }

    pub fn file_hotspots(
        &self,
        repo_id: i64,
        since_ts: i64,
        limit: i64,
    ) -> Result<Vec<FileHotspot>> {
        let read = self.db.begin_read()?;
        let by_repo_ts = read.open_table(FILE_CHANGES_BY_REPO_TS)?;
        let fc_t = read.open_table(FILE_CHANGES)?;
        let key_lo = (id_to_u64(repo_id), since_ts, 0u64);
        let key_hi = (id_to_u64(repo_id), i64::MAX, u64::MAX);

        struct Acc {
            churn: i64,
            commits: i64,
            authors: HashSet<String>,
        }
        let mut by_path: HashMap<String, Acc> = HashMap::new();
        for row in by_repo_ts.range(key_lo..=key_hi)? {
            let (k, _) = row?;
            let (_rid, _ts, fid) = k.value();
            let Some(g) = fc_t.get(fid)? else {
                continue;
            };
            let f: FileChangeRecord = serde_json::from_slice(g.value())?;
            let acc = by_path.entry(f.file_path.clone()).or_insert_with(|| Acc {
                churn: 0,
                commits: 0,
                authors: HashSet::new(),
            });
            acc.churn += f.additions + f.deletions;
            acc.commits += 1;
            acc.authors.insert(f.author_email);
        }
        let mut hotspots: Vec<FileHotspot> = by_path
            .into_iter()
            .map(|(p, a)| FileHotspot {
                file_path: p,
                churn: a.churn,
                commits: a.commits,
                authors: a.authors.len() as i64,
            })
            .collect();
        // ORDER BY churn DESC, commits DESC, then path ASC for stability.
        hotspots.sort_by(|a, b| {
            b.churn
                .cmp(&a.churn)
                .then_with(|| b.commits.cmp(&a.commits))
                .then_with(|| a.file_path.cmp(&b.file_path))
        });
        hotspots.truncate(limit.max(0) as usize);
        Ok(hotspots)
    }

    /// Read every record that needs to land in tantivy. The shape mirrors
    /// the prior FTS5 `search_idx` rows: `(kind, ref_id, text)`.
    pub fn collect_search_corpus(&self) -> Result<Vec<crate::search::IndexDoc>> {
        use crate::search::IndexDoc;
        let read = self.db.begin_read()?;
        let mut out: Vec<IndexDoc> = Vec::new();

        for row in read.open_table(REPOS)?.iter()? {
            let (_k, v) = row?;
            let r: Repo = serde_json::from_slice(v.value())?;
            let text = format!("{} {}", r.name, r.path);
            out.push(IndexDoc {
                kind: "repo".into(),
                ref_id: r.id,
                text,
            });
        }
        for row in read.open_table(SESSIONS)?.iter()? {
            let (_k, v) = row?;
            let s: Session = serde_json::from_slice(v.value())?;
            if let Some(text) = s.last_prompt {
                out.push(IndexDoc {
                    kind: "session".into(),
                    ref_id: s.id,
                    text,
                });
            }
        }
        for row in read.open_table(COMMITS)?.iter()? {
            let (_k, v) = row?;
            let c: Commit = serde_json::from_slice(v.value())?;
            out.push(IndexDoc {
                kind: "commit".into(),
                ref_id: c.id,
                text: c.subject,
            });
        }
        Ok(out)
    }

    pub fn uncloned_active_repos(&self, limit: i64) -> Result<Vec<ActiveRemoteRepo>> {
        let read = self.db.begin_read()?;
        let by_pushed = read.open_table(ACTIVE_REPOS_BY_PUSHED_AT)?;
        let active_t = read.open_table(ACTIVE_REMOTE_REPOS)?;
        let repos_by_remote: HashSet<String> = {
            let mut set = HashSet::new();
            for row in read.open_table(REPOS)?.iter()? {
                let (_k, v) = row?;
                let r: Repo = serde_json::from_slice(v.value())?;
                if let Some(u) = r.remote_url {
                    set.insert(u);
                }
            }
            set
        };
        // "Active" means pushed within the last 30 days. A repo high in the
        // top-100-by-pushedAt window with a years-old last push is not
        // meaningfully active, just less stale than the rest.
        let cutoff = chrono::Utc::now().timestamp() - 30 * 24 * 60 * 60;
        let mut out: Vec<ActiveRemoteRepo> = Vec::new();
        for row in by_pushed.iter()?.rev() {
            let (k, _) = row?;
            let (ts, aid) = k.value();
            if ts < cutoff {
                break;
            }
            let Some(g) = active_t.get(aid)? else {
                continue;
            };
            let a: ActiveRemoteRepo = serde_json::from_slice(g.value())?;
            if a.is_archived || a.is_fork {
                continue;
            }
            if repos_by_remote.contains(&a.https_url) {
                continue;
            }
            out.push(a);
            if out.len() as i64 >= limit {
                break;
            }
        }
        Ok(out)
    }

    pub fn get_active_repo_by_full_name(
        &self,
        full_name: &str,
    ) -> Result<Option<ActiveRemoteRepo>> {
        let read = self.db.begin_read()?;
        let by_name = read.open_table(ACTIVE_REPOS_BY_FULL_NAME)?;
        let active_t = read.open_table(ACTIVE_REMOTE_REPOS)?;
        let Some(g) = by_name.get(full_name)? else {
            return Ok(None);
        };
        let aid = g.value();
        match active_t.get(aid)? {
            Some(g) => Ok(Some(serde_json::from_slice(g.value())?)),
            None => Ok(None),
        }
    }

    /// Pull every repo in the cache as `(id, name)` pairs. Used by the
    /// content-mention pass to build the Aho-Corasick needle list.
    pub fn iter_repo_ids_and_names(&self) -> Result<Vec<(i64, String)>> {
        let read = self.db.begin_read()?;
        let mut out = Vec::new();
        for row in read.open_table(REPOS)?.iter()? {
            let (_k, v) = row?;
            let r: Repo = serde_json::from_slice(v.value())?;
            out.push((r.id, r.name));
        }
        Ok(out)
    }

    /// Pull every repo in the cache as `(id, remote_url)` pairs. Used by
    /// the gh-ref session-link pass to map `<owner>/<repo>` references in
    /// session text back onto a discovered repo's GitHub remote. Repos
    /// without a remote (or with a non-GitHub remote) are filtered out by
    /// the caller via `ingest::git::log::github_owner_repo`.
    pub fn iter_repo_ids_and_remotes(&self) -> Result<Vec<(i64, String)>> {
        let read = self.db.begin_read()?;
        let mut out = Vec::new();
        for row in read.open_table(REPOS)?.iter()? {
            let (_k, v) = row?;
            let r: Repo = serde_json::from_slice(v.value())?;
            if let Some(url) = r.remote_url {
                out.push((r.id, url));
            }
        }
        Ok(out)
    }

    /// Compute the AFK success-rate rollup (#92, #116). Walks every
    /// repo-dispatch tracking issue (open + closed) and classifies the
    /// closed ones as success / abandon / block by joining against
    /// the issue-ref table.
    ///
    /// Classification:
    /// - `successes` — closed and at least one commit in `ISSUE_REFS`
    ///   references this issue (auto-close trailer landed real code).
    /// - `abandons` — closed with no referencing commit (manual close
    ///   without a fix).
    /// - `blocks` — closed but at least one open `autonomous-block`
    ///   issue exists in the same repo created within 7 days of the
    ///   close. Lossy heuristic; refine later via dispatch-file link.
    /// - `open` — still open.
    pub fn autonomy_metrics(&self) -> Result<AutonomyMetrics> {
        let open = self.labeled_issues_by_state("repo-dispatch", "open")?;
        let closed = self.labeled_issues_by_state("repo-dispatch", "closed")?;
        let blocks = self.labeled_issues_by_state("autonomous-block", "open")?;
        let block_windows: BTreeMap<i64, Vec<i64>> =
            blocks
                .iter()
                .fold(BTreeMap::new(), |mut acc: BTreeMap<i64, Vec<i64>>, b| {
                    acc.entry(b.repo_id).or_default().push(b.created_at);
                    acc
                });

        let mut per_repo: BTreeMap<i64, DispatchBucket> = BTreeMap::new();
        for o in &open {
            let b = per_repo.entry(o.repo_id).or_default();
            b.open += 1;
            b.total += 1;
        }
        for c in &closed {
            let history = self.ticket_history(c.repo_id, c.number as u32)?;
            let close_ts = c.closed_at.unwrap_or(c.created_at);
            let blocked_window_secs = 7 * 86_400;
            let blocked_nearby = block_windows
                .get(&c.repo_id)
                .map(|ts| {
                    ts.iter()
                        .any(|bt| (close_ts - bt).abs() < blocked_window_secs)
                })
                .unwrap_or(false);
            let b = per_repo.entry(c.repo_id).or_default();
            if !history.commits.is_empty() {
                b.successes += 1;
            } else if blocked_nearby {
                b.blocks += 1;
            } else {
                b.abandons += 1;
            }
            b.total += 1;
        }

        // Build per-repo with names.
        let name_lookup: BTreeMap<i64, String> = self
            .list_repos_with_counts()?
            .into_iter()
            .map(|r| (r.id, r.name))
            .collect();
        let mut per_repo_out: Vec<RepoDispatchMetrics> = per_repo
            .into_iter()
            .map(|(repo_id, bucket)| {
                let success_rate = bucket.success_rate();
                RepoDispatchMetrics {
                    repo_id,
                    repo_name: name_lookup
                        .get(&repo_id)
                        .cloned()
                        .unwrap_or_else(|| format!("repo {repo_id}")),
                    bucket,
                    success_rate,
                }
            })
            .collect();
        per_repo_out.sort_by_key(|r| std::cmp::Reverse(r.bucket.total));

        let overall = per_repo_out
            .iter()
            .fold(DispatchBucket::default(), |mut acc, r| {
                acc.successes += r.bucket.successes;
                acc.abandons += r.bucket.abandons;
                acc.blocks += r.bucket.blocks;
                acc.open += r.bucket.open;
                acc.total += r.bucket.total;
                acc
            });
        let overall_success_rate = overall.success_rate();

        Ok(AutonomyMetrics {
            overall,
            overall_success_rate,
            per_repo: per_repo_out,
        })
    }

    /// Per-repo signal entries derived from the labeled-issues table.
    /// Emits at most one entry per `(repo, signal)` to match the
    /// existing `recall_action_required` shape:
    ///
    /// - `autonomous_block` — N>=1 open `autonomous-block` issues.
    /// - `stale_ask` — N>=1 open `structural-ask` issues older than
    ///   `stale_after_secs`.
    ///
    /// Designed in #92, #115. Detail strings include the oldest issue
    /// number so the dashboard can deep-link to the one most worth
    /// resolving.
    pub fn dispatch_signals(&self, stale_after_secs: i64) -> Result<Vec<DispatchSignal>> {
        let blocks = self.labeled_issues_by_state("autonomous-block", "open")?;
        let asks = self.labeled_issues_by_state("structural-ask", "open")?;
        let now = chrono::Utc::now().timestamp();
        let cutoff = now - stale_after_secs;

        let mut by_repo_blocks: BTreeMap<i64, Vec<LabeledIssueRow>> = BTreeMap::new();
        for b in blocks {
            by_repo_blocks.entry(b.repo_id).or_default().push(b);
        }
        let mut by_repo_stale: BTreeMap<i64, Vec<LabeledIssueRow>> = BTreeMap::new();
        for a in asks {
            if a.created_at > 0 && a.created_at < cutoff {
                by_repo_stale.entry(a.repo_id).or_default().push(a);
            }
        }

        let mut out = Vec::new();
        for (repo_id, mut rows) in by_repo_blocks {
            rows.sort_by_key(|r| r.created_at);
            let oldest = &rows[0];
            let n = rows.len();
            out.push(DispatchSignal {
                repo_id,
                signal: "autonomous_block",
                detail: format!(
                    "{n} autonomous-block issue{} open (oldest: #{})",
                    if n == 1 { "" } else { "s" },
                    oldest.number,
                ),
            });
        }
        for (repo_id, mut rows) in by_repo_stale {
            rows.sort_by_key(|r| r.created_at);
            let oldest = &rows[0];
            let n = rows.len();
            let days = (now - oldest.created_at) / 86_400;
            out.push(DispatchSignal {
                repo_id,
                signal: "stale_ask",
                detail: format!(
                    "{n} structural-ask{} open >{} days (oldest: #{}, {days}d)",
                    if n == 1 { "" } else { "s" },
                    stale_after_secs / 86_400,
                    oldest.number,
                ),
            });
        }
        Ok(out)
    }

    /// Cross-repo view: every labeled issue matching `label` (lowercased)
    /// and `state` ("open" / "closed"). Used by the structural-asks and
    /// autonomous-blocks panels (#92, #114, #115).
    pub fn labeled_issues_by_state(
        &self,
        label: &str,
        state: &str,
    ) -> Result<Vec<LabeledIssueRow>> {
        let read = self.db.begin_read()?;
        let by_label_state = read.open_table(LABELED_ISSUES_BY_LABEL_STATE)?;
        let issues_t = read.open_table(LABELED_ISSUES)?;
        let key_t = read.open_table(LABELED_ISSUES_BY_REPO_LABEL_NUMBER)?;
        let label_lc = label.to_ascii_lowercase();
        let state_lc = state.to_ascii_lowercase();
        let start = (label_lc.as_str(), state_lc.as_str(), 0u64, i64::MIN);
        let end = (label_lc.as_str(), state_lc.as_str(), u64::MAX, i64::MAX);
        let mut out = Vec::new();
        for row in by_label_state.range(start..end)? {
            let (k, _v) = row?;
            let (_label, _state, repo_id, number) = k.value();
            let key = (repo_id, label_lc.as_str(), number);
            if let Some(g) = key_t.get(key)? {
                if let Some(row_g) = issues_t.get(g.value())? {
                    out.push(serde_json::from_slice(row_g.value())?);
                }
            }
        }
        out.sort_by_key(|r: &LabeledIssueRow| std::cmp::Reverse(r.created_at));
        Ok(out)
    }

    /// Per-repo view: every labeled issue matching `label` for one repo.
    pub fn labeled_issues_for_repo(
        &self,
        repo_id: i64,
        label: &str,
    ) -> Result<Vec<LabeledIssueRow>> {
        let read = self.db.begin_read()?;
        let by_key = read.open_table(LABELED_ISSUES_BY_REPO_LABEL_NUMBER)?;
        let issues_t = read.open_table(LABELED_ISSUES)?;
        let label_lc = label.to_ascii_lowercase();
        let start = (id_to_u64(repo_id), label_lc.as_str(), i64::MIN);
        let end = (id_to_u64(repo_id), label_lc.as_str(), i64::MAX);
        let mut out = Vec::new();
        for row in by_key.range(start..end)? {
            let (_k, v) = row?;
            if let Some(g) = issues_t.get(v.value())? {
                out.push(serde_json::from_slice(g.value())?);
            }
        }
        out.sort_by_key(|r: &LabeledIssueRow| std::cmp::Reverse(r.created_at));
        Ok(out)
    }

    /// Return all dispatch records for a repo, newest-first by
    /// `dispatched_at`. Empty when the repo has no `docs/repo-dispatch/`
    /// or all files failed to parse.
    pub fn dispatches_for_repo(&self, repo_id: i64) -> Result<Vec<DispatchRow>> {
        let read = self.db.begin_read()?;
        let by_repo = read.open_table(DISPATCHES_BY_REPO)?;
        let dispatches = read.open_table(DISPATCHES)?;
        let start = (id_to_u64(repo_id), i64::MIN, 0u64);
        let end = (id_to_u64(repo_id) + 1, i64::MIN, 0u64);
        let mut out = Vec::new();
        for row in by_repo.range(start..end)? {
            let (k, _v) = row?;
            let (_rid, _sort, did) = k.value();
            if let Some(g) = dispatches.get(did)? {
                out.push(serde_json::from_slice(g.value())?);
            }
        }
        Ok(out)
    }

    /// Return the sessions and commits in the cache that reference a
    /// given issue in a given repo. Walks the `ISSUE_REFS_BY_REPO_ISSUE`
    /// secondary index, then loads source records. Empty result is the
    /// expected normal case for unindexed tickets — never errors.
    ///
    /// Used by `recall_ticket_history` (#92) to ground per-ticket
    /// dispatch context in real prior work.
    pub fn ticket_history(&self, repo_id: i64, issue_number: u32) -> Result<TicketHistory> {
        let read = self.db.begin_read()?;
        let by_repo_issue = read.open_table(ISSUE_REFS_BY_REPO_ISSUE)?;
        let sessions_t = read.open_table(SESSIONS)?;
        let commits_t = read.open_table(COMMITS)?;
        let start = (id_to_u64(repo_id), issue_number, "", 0u64);
        let end = (id_to_u64(repo_id), issue_number + 1, "", 0u64);
        let mut sessions = Vec::new();
        let mut commits = Vec::new();
        for row in by_repo_issue.range(start..end)? {
            let (k, _v) = row?;
            let (_rid, _iss, source_kind, source_id) = k.value();
            match source_kind {
                issue_ref_source::SESSION => {
                    if let Some(g) = sessions_t.get(source_id)? {
                        sessions.push(serde_json::from_slice(g.value())?);
                    }
                }
                issue_ref_source::COMMIT => {
                    if let Some(g) = commits_t.get(source_id)? {
                        commits.push(serde_json::from_slice(g.value())?);
                    }
                }
                _ => {}
            }
        }
        // Newest-first within each list keeps the dashboard view sensible
        // without forcing the caller to re-sort.
        sessions.sort_by_key(|s: &Session| std::cmp::Reverse(s.started_at.unwrap_or(i64::MIN)));
        commits.sort_by_key(|c: &Commit| std::cmp::Reverse(c.timestamp));
        Ok(TicketHistory {
            repo_id,
            issue_number,
            sessions,
            commits,
        })
    }

    /// Pull every indexed session as `(id, source_file)` pairs for the
    /// content-mention scan to drive its file walk.
    pub fn iter_session_source_files(&self) -> Result<Vec<(i64, String)>> {
        let read = self.db.begin_read()?;
        let mut out = Vec::new();
        for row in read.open_table(SESSIONS)?.iter()? {
            let (_k, v) = row?;
            let s: Session = serde_json::from_slice(v.value())?;
            out.push((s.id, s.source_file));
        }
        Ok(out)
    }

    /// Subset of repos eligible for remote-state queries: those with a
    /// known origin URL and default branch. Sorted by most-recent commit
    /// timestamp DESC so the optional cap keeps the active workspace.
    pub fn remote_targets(
        &self,
        target_limit: usize,
    ) -> Result<Vec<(i64, String, String, String)>> {
        let read = self.db.begin_read()?;
        let mut latest_per_repo: HashMap<u64, i64> = HashMap::new();
        for row in read.open_table(COMMITS_BY_REPO_TS)?.iter()? {
            let (k, _) = row?;
            let (rid, ts, _) = k.value();
            let entry = latest_per_repo.entry(rid).or_insert(i64::MIN);
            if ts > *entry {
                *entry = ts;
            }
        }
        let mut all: Vec<(i64, String, String, String, i64)> = Vec::new();
        for row in read.open_table(REPOS)?.iter()? {
            let (_k, v) = row?;
            let r: Repo = serde_json::from_slice(v.value())?;
            let (Some(url), Some(branch)) = (r.remote_url, r.default_branch) else {
                continue;
            };
            let latest = *latest_per_repo.get(&id_to_u64(r.id)).unwrap_or(&i64::MIN);
            all.push((r.id, url, branch, r.path, latest));
        }
        all.sort_by_key(|r| std::cmp::Reverse(r.4));
        let trimmed: Vec<(i64, String, String, String)> = all
            .into_iter()
            .take(if target_limit == 0 {
                usize::MAX
            } else {
                target_limit
            })
            .map(|(id, url, branch, path, _)| (id, url, branch, path))
            .collect();
        Ok(trimmed)
    }
}

// ---------------------------------------------------------------------------
// CacheWriter: a typed handle on an open redb write transaction
// ---------------------------------------------------------------------------

pub struct CacheWriter<'a> {
    txn: &'a WriteTransaction,
}

impl CacheWriter<'_> {
    pub fn wipe(&self) -> Result<()> {
        clear_table::<u64, &[u8]>(self.txn, REPOS)?;
        clear_table::<u64, &[u8]>(self.txn, SESSIONS)?;
        clear_table::<u64, &[u8]>(self.txn, COMMITS)?;
        clear_table::<u64, &[u8]>(self.txn, FILE_CHANGES)?;
        clear_table::<u64, &[u8]>(self.txn, UNCOMMITTED_FILES)?;
        clear_table::<u64, &[u8]>(self.txn, ACTIVE_REMOTE_REPOS)?;
        clear_table::<&str, u64>(self.txn, META)?;
        clear_table::<&str, u64>(self.txn, REPOS_BY_PATH)?;
        clear_table::<&str, u64>(self.txn, SESSIONS_BY_UUID)?;
        clear_table::<(i64, u64), ()>(self.txn, SESSIONS_BY_STARTED_AT)?;
        clear_table::<(u64, u64, &str), ()>(self.txn, SESSION_REPOS)?;
        clear_table::<(u64, u64, &str), ()>(self.txn, SESSION_REPOS_BY_REPO)?;
        clear_table::<(u64, &str), u64>(self.txn, COMMITS_BY_REPO_SHA)?;
        clear_table::<(u64, i64, u64), &str>(self.txn, COMMITS_BY_REPO_TS)?;
        clear_table::<(i64, u64), ()>(self.txn, COMMITS_BY_TS)?;
        clear_table::<(u64, i64, u64), ()>(self.txn, FILE_CHANGES_BY_REPO_TS)?;
        clear_table::<(u64, u64), ()>(self.txn, UNCOMMITTED_BY_REPO)?;
        clear_table::<&str, u64>(self.txn, ACTIVE_REPOS_BY_FULL_NAME)?;
        clear_table::<&str, u64>(self.txn, ACTIVE_REPOS_BY_HTTPS_URL)?;
        clear_table::<(i64, u64), ()>(self.txn, ACTIVE_REPOS_BY_PUSHED_AT)?;
        clear_table::<u64, &[u8]>(self.txn, AUDIT_EVENTS)?;
        clear_table::<(u64, i64, u64), ()>(self.txn, AUDIT_EVENTS_BY_REPO_TS)?;
        clear_table::<&str, u64>(self.txn, AUDIT_EVENTS_BY_NATURAL_KEY)?;
        clear_table::<u64, &[u8]>(self.txn, ISSUE_REFS)?;
        clear_table::<(u64, u32, &str, u64), u64>(self.txn, ISSUE_REFS_BY_REPO_ISSUE)?;
        clear_table::<(&str, u64, u64, u32), ()>(self.txn, ISSUE_REFS_BY_SOURCE)?;
        clear_table::<u64, &[u8]>(self.txn, DISPATCHES)?;
        clear_table::<(u64, i64, u64), ()>(self.txn, DISPATCHES_BY_REPO)?;
        clear_table::<u64, &[u8]>(self.txn, LABELED_ISSUES)?;
        clear_table::<(u64, &str, i64), u64>(self.txn, LABELED_ISSUES_BY_REPO_LABEL_NUMBER)?;
        clear_table::<(&str, &str, u64, i64), ()>(self.txn, LABELED_ISSUES_BY_LABEL_STATE)?;
        Ok(())
    }

    /// Insert a repo or return the existing id if `path` is already
    /// present. Mirrors the SQLite `INSERT OR IGNORE ... SELECT id`.
    pub fn upsert_repo(
        &self,
        path: &str,
        name: &str,
        discovered_at: i64,
        remote_url: Option<&str>,
        default_branch: Option<&str>,
    ) -> Result<i64> {
        let mut by_path = self.txn.open_table(REPOS_BY_PATH)?;
        if let Some(g) = by_path.get(path)? {
            return Ok(u64_to_id(g.value()));
        }
        let id = next_id(self.txn, META_NEXT_REPO)?;
        let repo = Repo {
            id: u64_to_id(id),
            path: path.into(),
            name: name.into(),
            session_count: 0,
            commits_30d: 0,
            loc_churn_30d: 0,
            untracked_files: 0,
            modified_files: 0,
            authors_30d: 0,
            ci_status: None,
            commits_ahead: 0,
            commits_behind: 0,
            stash_count: 0,
            head_ref: None,
            in_progress_op: None,
            open_prs: 0,
            draft_prs: 0,
            open_issues: 0,
            prs_awaiting_my_review: 0,
            prs_mine_awaiting_review: 0,
            prs_mine_no_reviewer: 0,
            my_draft_prs: 0,
            issues_assigned_to_me: 0,
            deploy_workflow: None,
            deploy_status: None,
            deploy_last_success_ts: None,
            remote_url: remote_url.map(str::to_string),
            default_branch: default_branch.map(str::to_string),
        };
        let _ = discovered_at; // discovery time is not surfaced anywhere
        let bytes = serde_json::to_vec(&repo)?;
        let mut repos = self.txn.open_table(REPOS)?;
        repos.insert(id, bytes.as_slice())?;
        by_path.insert(path, id)?;
        Ok(u64_to_id(id))
    }

    /// Insert a session if its UUID has not been seen, returning the
    /// `(session_id, true)` on success or `(existing_id, false)` if a
    /// previous file already produced this UUID.
    pub fn upsert_session(
        &self,
        rec: &crate::ingest::claude::sessions_jsonl::SessionRecord,
    ) -> Result<(i64, bool)> {
        let mut by_uuid = self.txn.open_table(SESSIONS_BY_UUID)?;
        if let Some(g) = by_uuid.get(rec.session_uuid.as_str())? {
            return Ok((u64_to_id(g.value()), false));
        }
        let id = next_id(self.txn, META_NEXT_SESSION)?;
        let session = Session {
            id: u64_to_id(id),
            session_uuid: rec.session_uuid.clone(),
            cwd: rec.cwd.clone(),
            started_at: rec.started_at,
            ended_at: rec.ended_at,
            message_count: rec.message_count,
            user_message_count: rec.user_message_count,
            assistant_message_count: rec.assistant_message_count,
            last_prompt: rec.last_prompt.clone(),
            source_file: rec.source_file.clone(),
            duration_ms: rec.duration_ms,
            input_tokens: rec.input_tokens,
            output_tokens: rec.output_tokens,
            cache_read_tokens: rec.cache_read_tokens,
            cache_creation_tokens: rec.cache_creation_tokens,
            parent_uuid: rec.parent_uuid.clone(),
            request_id: rec.request_id.clone(),
            message_id: rec.message_id.clone(),
            is_sidechain_count: rec.is_sidechain_count,
            models_used: rec.models_used.clone(),
            tools_used: rec.tools_used.clone(),
            tool_call_counts_json: rec.tool_call_counts_json.clone(),
            stop_reason_counts_json: rec.stop_reason_counts_json.clone(),
        };
        let bytes = serde_json::to_vec(&session)?;
        self.txn
            .open_table(SESSIONS)?
            .insert(id, bytes.as_slice())?;
        by_uuid.insert(rec.session_uuid.as_str(), id)?;
        let ts_key = rec.started_at.unwrap_or(i64::MIN);
        self.txn
            .open_table(SESSIONS_BY_STARTED_AT)?
            .insert((ts_key, id), ())?;
        Ok((u64_to_id(id), true))
    }

    /// Insert one audit event keyed by its `event_id` (uuid7 from cli-guard).
    /// Returns `(id, true)` on first sight, `(existing_id, false)` on dup.
    /// `repo_id == 0` is allowed and reserved for rows whose `commit_scope`
    /// didn't match any discovered repo. See #148.
    pub fn upsert_audit_event(
        &self,
        repo_id: i64,
        rec: &crate::ingest::cli_guard::audit_jsonl::AuditRecord,
    ) -> Result<(i64, bool)> {
        let mut by_natural = self.txn.open_table(AUDIT_EVENTS_BY_NATURAL_KEY)?;
        if let Some(g) = by_natural.get(rec.event_id.as_str())? {
            return Ok((u64_to_id(g.value()), false));
        }
        let id = next_id(self.txn, META_NEXT_AUDIT_EVENT)?;
        let evt = AuditEvent {
            id: u64_to_id(id),
            repo_id,
            event_id: rec.event_id.clone(),
            ts: rec.ts,
            decision: rec.decision.clone(),
            verb: rec.verb.clone(),
            argv: rec.argv.clone(),
            exit_code: rec.exit_code,
            duration_ms: rec.duration_ms,
            commit_scope: rec.commit_scope.clone(),
            audit_override: rec.audit_override,
            source_file: rec.source_file.clone(),
            session_id: rec.session_id.clone(),
            version: rec.version.clone(),
            error: rec.error.clone(),
            stderr_tail: rec.stderr_tail.clone(),
            repo_root: rec.repo_root.clone(),
            cwd_subprocess: rec.cwd_subprocess.clone(),
            cwd_at_invocation: rec.cwd_at_invocation.clone(),
            egress: rec.egress.clone(),
            profile_decision: rec.profile_decision.clone(),
        };
        let bytes = serde_json::to_vec(&evt)?;
        self.txn
            .open_table(AUDIT_EVENTS)?
            .insert(id, bytes.as_slice())?;
        by_natural.insert(rec.event_id.as_str(), id)?;
        self.txn
            .open_table(AUDIT_EVENTS_BY_REPO_TS)?
            .insert((id_to_u64(repo_id), -rec.ts, id), ())?;
        Ok((u64_to_id(id), true))
    }

    /// Add `(session_id, repo_id, match_type)` to the join. Returns true
    /// when the row was new, mirroring `INSERT OR IGNORE` row-count.
    pub fn link_session_repo(
        &self,
        session_id: i64,
        repo_id: i64,
        match_type: &str,
    ) -> Result<bool> {
        let mut t = self.txn.open_table(SESSION_REPOS)?;
        let mut by_repo = self.txn.open_table(SESSION_REPOS_BY_REPO)?;
        let key = (id_to_u64(session_id), id_to_u64(repo_id), match_type);
        if t.get(key)?.is_some() {
            return Ok(false);
        }
        t.insert(key, ())?;
        by_repo.insert((id_to_u64(repo_id), id_to_u64(session_id), match_type), ())?;
        Ok(true)
    }

    /// Insert a commit if `(repo_id, sha)` is new. Returns
    /// `(commit_id, true)` on first sight, `(existing_id, false)` on dup.
    /// Callers use the id to wire downstream side-tables (e.g. issue refs).
    #[allow(clippy::too_many_arguments)]
    pub fn upsert_commit(
        &self,
        repo_id: i64,
        sha: &str,
        author_name: &str,
        author_email: &str,
        timestamp: i64,
        subject: &str,
    ) -> Result<(i64, bool)> {
        let mut by_sha = self.txn.open_table(COMMITS_BY_REPO_SHA)?;
        if let Some(g) = by_sha.get((id_to_u64(repo_id), sha))? {
            return Ok((u64_to_id(g.value()), false));
        }
        let id = next_id(self.txn, META_NEXT_COMMIT)?;
        let commit = Commit {
            id: u64_to_id(id),
            repo_id,
            sha: sha.into(),
            author_name: author_name.into(),
            author_email: author_email.into(),
            timestamp,
            subject: subject.into(),
        };
        let bytes = serde_json::to_vec(&commit)?;
        self.txn.open_table(COMMITS)?.insert(id, bytes.as_slice())?;
        by_sha.insert((id_to_u64(repo_id), sha), id)?;
        self.txn
            .open_table(COMMITS_BY_REPO_TS)?
            .insert((id_to_u64(repo_id), timestamp, id), author_email)?;
        self.txn
            .open_table(COMMITS_BY_TS)?
            .insert((timestamp, id), ())?;
        Ok((u64_to_id(id), true))
    }

    /// Insert (or replace) one labeled-issue row in the cache.
    /// `label` is forced lowercase for index stability; the original
    /// case is preserved in the row's `labels` vector. Idempotent on
    /// `(repo_id, label, number)`.
    pub fn upsert_labeled_issue(
        &self,
        repo_id: i64,
        label: &str,
        issue: &crate::ingest::git::log::LabeledIssue,
    ) -> Result<i64> {
        let label_lc = label.to_ascii_lowercase();
        let mut by_key = self.txn.open_table(LABELED_ISSUES_BY_REPO_LABEL_NUMBER)?;
        let key = (id_to_u64(repo_id), label_lc.as_str(), issue.number);
        if let Some(g) = by_key.get(key)? {
            return Ok(u64_to_id(g.value()));
        }
        let id = next_id(self.txn, META_NEXT_LABELED_ISSUE)?;
        let row = LabeledIssueRow {
            id: u64_to_id(id),
            repo_id,
            label: label_lc.clone(),
            number: issue.number,
            title: issue.title.clone(),
            created_at: issue.created_at,
            closed_at: issue.closed_at,
            state: issue.state.clone(),
            labels: issue.labels.clone(),
        };
        let bytes = serde_json::to_vec(&row)?;
        self.txn
            .open_table(LABELED_ISSUES)?
            .insert(id, bytes.as_slice())?;
        by_key.insert(key, id)?;
        let state_lc = issue.state.to_ascii_lowercase();
        self.txn.open_table(LABELED_ISSUES_BY_LABEL_STATE)?.insert(
            (
                label_lc.as_str(),
                state_lc.as_str(),
                id_to_u64(repo_id),
                issue.number,
            ),
            (),
        )?;
        Ok(u64_to_id(id))
    }

    /// Insert one parsed dispatch row into the cache. Each `(repo_id,
    /// file_path)` pair must be unique — the caller controls this by
    /// scanning a single directory per repo. Returns the assigned id.
    pub fn insert_dispatch(
        &self,
        repo_id: i64,
        rec: &crate::ingest::docs::repo_dispatch::DispatchRecord,
    ) -> Result<i64> {
        let id = next_id(self.txn, META_NEXT_DISPATCH)?;
        let row = DispatchRow {
            id: u64_to_id(id),
            repo_id,
            file_path: rec.file_path.clone(),
            slug: rec.slug.clone(),
            issue_refs: rec.issue_refs.clone(),
            score: rec.score,
            autonomy_confidence: rec.autonomy_confidence,
            autonomy_confidence_basis: rec.autonomy_confidence_basis.clone(),
            prompt_hash: rec.prompt_hash.clone(),
            dispatched_at: rec.dispatched_at,
            tracking_issue: rec.tracking_issue.clone(),
        };
        let bytes = serde_json::to_vec(&row)?;
        self.txn
            .open_table(DISPATCHES)?
            .insert(id, bytes.as_slice())?;
        // Negate dispatched_at so a forward scan returns newest-first;
        // rows without a timestamp use i64::MAX so they sort last.
        let sort_key = match rec.dispatched_at {
            Some(t) => -t,
            None => i64::MAX,
        };
        self.txn
            .open_table(DISPATCHES_BY_REPO)?
            .insert((id_to_u64(repo_id), sort_key, id), ())?;
        Ok(u64_to_id(id))
    }

    /// Record that a session or commit references a specific issue in a
    /// known repo. Idempotent on `(repo_id, issue_number, source_kind,
    /// source_id)`. Returns true on first sight. `source_kind` is one of
    /// `IssueRefSource::SESSION` / `IssueRefSource::COMMIT`.
    ///
    /// Designed for the recall-dispatch substrate reads spec'd in #92:
    /// `recall_ticket_history` walks this table to ground per-ticket
    /// context in real prior work.
    pub fn record_issue_ref(
        &self,
        repo_id: i64,
        issue_number: u32,
        source_kind: &str,
        source_id: i64,
    ) -> Result<bool> {
        let mut by_repo_issue = self.txn.open_table(ISSUE_REFS_BY_REPO_ISSUE)?;
        let key = (
            id_to_u64(repo_id),
            issue_number,
            source_kind,
            id_to_u64(source_id),
        );
        if by_repo_issue.get(key)?.is_some() {
            return Ok(false);
        }
        let id = next_id(self.txn, META_NEXT_ISSUE_REF)?;
        let rec = IssueRef {
            id: u64_to_id(id),
            repo_id,
            issue_number,
            source_kind: source_kind.into(),
            source_id,
        };
        let bytes = serde_json::to_vec(&rec)?;
        self.txn
            .open_table(ISSUE_REFS)?
            .insert(id, bytes.as_slice())?;
        by_repo_issue.insert(key, id)?;
        self.txn.open_table(ISSUE_REFS_BY_SOURCE)?.insert(
            (
                source_kind,
                id_to_u64(source_id),
                id_to_u64(repo_id),
                issue_number,
            ),
            (),
        )?;
        Ok(true)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn insert_file_change(
        &self,
        repo_id: i64,
        sha: &str,
        file_path: &str,
        additions: i64,
        deletions: i64,
        author_email: &str,
        timestamp: i64,
    ) -> Result<()> {
        let id = next_id(self.txn, META_NEXT_FILE_CHANGE)?;
        let rec = FileChangeRecord {
            id: u64_to_id(id),
            repo_id,
            sha: sha.into(),
            file_path: file_path.into(),
            additions,
            deletions,
            author_email: author_email.into(),
            timestamp,
        };
        let bytes = serde_json::to_vec(&rec)?;
        self.txn
            .open_table(FILE_CHANGES)?
            .insert(id, bytes.as_slice())?;
        self.txn
            .open_table(FILE_CHANGES_BY_REPO_TS)?
            .insert((id_to_u64(repo_id), timestamp, id), ())?;
        Ok(())
    }

    pub fn insert_uncommitted_file(&self, repo_id: i64, path: &str, kind: &str) -> Result<()> {
        let id = next_id(self.txn, META_NEXT_UNCOMMITTED)?;
        let rec = UncommittedFileRecord {
            id: u64_to_id(id),
            repo_id,
            path: path.into(),
            kind: kind.into(),
        };
        let bytes = serde_json::to_vec(&rec)?;
        self.txn
            .open_table(UNCOMMITTED_FILES)?
            .insert(id, bytes.as_slice())?;
        self.txn
            .open_table(UNCOMMITTED_BY_REPO)?
            .insert((id_to_u64(repo_id), id), ())?;
        Ok(())
    }

    /// Update a repo's local-state fields. Mirrors the SQLite UPDATE that
    /// runs after the per-repo git scan.
    #[allow(clippy::too_many_arguments)]
    pub fn update_repo_local_state(
        &self,
        repo_id: i64,
        loc_churn_30d: i64,
        untracked_files: i64,
        modified_files: i64,
        commits_ahead: i64,
        commits_behind: i64,
        stash_count: i64,
        head_ref: Option<&str>,
        in_progress_op: Option<&str>,
    ) -> Result<()> {
        self.mutate_repo(repo_id, |r| {
            r.loc_churn_30d = loc_churn_30d;
            r.untracked_files = untracked_files;
            r.modified_files = modified_files;
            r.commits_ahead = commits_ahead;
            r.commits_behind = commits_behind;
            r.stash_count = stash_count;
            r.head_ref = head_ref.map(str::to_string);
            r.in_progress_op = in_progress_op.map(str::to_string);
        })
    }

    /// Update a repo's remote-state fields. `None` arguments preserve the
    /// existing value (matching the SQLite COALESCE semantics).
    #[allow(clippy::too_many_arguments)]
    pub fn update_repo_remote_state(
        &self,
        repo_id: i64,
        ci_status: Option<String>,
        open_prs: i64,
        draft_prs: i64,
        prs_awaiting_my_review: i64,
        prs_mine_awaiting_review: i64,
        prs_mine_no_reviewer: i64,
        my_draft_prs: i64,
        open_issues: Option<i64>,
        issues_assigned_to_me: Option<i64>,
        deploy_workflow: Option<String>,
        deploy_status: Option<String>,
        deploy_last_success_ts: Option<i64>,
    ) -> Result<()> {
        self.mutate_repo(repo_id, |r| {
            if ci_status.is_some() {
                r.ci_status = ci_status;
            }
            r.open_prs = open_prs;
            r.draft_prs = draft_prs;
            r.prs_awaiting_my_review = prs_awaiting_my_review;
            r.prs_mine_awaiting_review = prs_mine_awaiting_review;
            r.prs_mine_no_reviewer = prs_mine_no_reviewer;
            r.my_draft_prs = my_draft_prs;
            if let Some(v) = open_issues {
                r.open_issues = v;
            }
            if let Some(v) = issues_assigned_to_me {
                r.issues_assigned_to_me = v;
            }
            if deploy_workflow.is_some() {
                r.deploy_workflow = deploy_workflow;
            }
            if deploy_status.is_some() {
                r.deploy_status = deploy_status;
            }
            if let Some(v) = deploy_last_success_ts {
                r.deploy_last_success_ts = Some(v);
            }
        })
    }

    /// Recompute and store the per-repo aggregates that the dashboard
    /// reads back in one shot: `session_count`, `commits_30d`,
    /// `authors_30d`. Run once at the end of every refresh after the row
    /// inserts and per-repo state updates have all landed.
    pub fn finalize_repo_aggregates(&self, cutoff_30d_ts: i64) -> Result<()> {
        // Tally per-repo session counts (DISTINCT session_id).
        let mut sessions_per_repo: BTreeMap<u64, BTreeSet<u64>> = BTreeMap::new();
        {
            let by_repo = self.txn.open_table(SESSION_REPOS_BY_REPO)?;
            for row in by_repo.iter()? {
                let (k, _) = row?;
                let (rid, sid, _mt) = k.value();
                sessions_per_repo.entry(rid).or_default().insert(sid);
            }
        }
        // Tally commits_30d + authors_30d per repo.
        let mut commits_per_repo: HashMap<u64, i64> = HashMap::new();
        let mut authors_per_repo: HashMap<u64, HashSet<String>> = HashMap::new();
        {
            let by_repo_ts = self.txn.open_table(COMMITS_BY_REPO_TS)?;
            for row in by_repo_ts.iter()? {
                let (k, v) = row?;
                let (rid, ts, _cid) = k.value();
                if ts < cutoff_30d_ts {
                    continue;
                }
                *commits_per_repo.entry(rid).or_insert(0) += 1;
                authors_per_repo
                    .entry(rid)
                    .or_default()
                    .insert(v.value().to_string());
            }
        }

        // Iterate every repo and write back its aggregates.
        let ids: Vec<u64> = {
            let repos = self.txn.open_table(REPOS)?;
            let mut ids = Vec::new();
            for row in repos.iter()? {
                let (k, _) = row?;
                ids.push(k.value());
            }
            ids
        };
        for id in ids {
            self.mutate_repo(u64_to_id(id), |r| {
                r.session_count = sessions_per_repo
                    .get(&id)
                    .map(|s| s.len() as i64)
                    .unwrap_or(0);
                r.commits_30d = *commits_per_repo.get(&id).unwrap_or(&0);
                r.authors_30d = authors_per_repo
                    .get(&id)
                    .map(|s| s.len() as i64)
                    .unwrap_or(0);
            })?;
        }
        Ok(())
    }

    /// Replace the active-remote-repos snapshot with `repos`. Wipes the
    /// table first to match `DELETE FROM active_remote_repos` + bulk
    /// insert. Returns the number of rows written.
    pub fn replace_active_remote_repos(&self, repos: &[ActiveRemoteRepo]) -> Result<usize> {
        clear_table::<u64, &[u8]>(self.txn, ACTIVE_REMOTE_REPOS)?;
        clear_table::<&str, u64>(self.txn, ACTIVE_REPOS_BY_FULL_NAME)?;
        clear_table::<&str, u64>(self.txn, ACTIVE_REPOS_BY_HTTPS_URL)?;
        clear_table::<(i64, u64), ()>(self.txn, ACTIVE_REPOS_BY_PUSHED_AT)?;

        let mut active = self.txn.open_table(ACTIVE_REMOTE_REPOS)?;
        let mut by_full_name = self.txn.open_table(ACTIVE_REPOS_BY_FULL_NAME)?;
        let mut by_https = self.txn.open_table(ACTIVE_REPOS_BY_HTTPS_URL)?;
        let mut by_pushed = self.txn.open_table(ACTIVE_REPOS_BY_PUSHED_AT)?;
        let mut written = 0usize;
        for r in repos {
            // Match the SQLite `INSERT OR IGNORE` on full_name.
            if by_full_name.get(r.full_name.as_str())?.is_some() {
                continue;
            }
            let id = next_id(self.txn, META_NEXT_ACTIVE_REPO)?;
            let rec = ActiveRemoteRepo {
                id: u64_to_id(id),
                ..r.clone()
            };
            let bytes = serde_json::to_vec(&rec)?;
            active.insert(id, bytes.as_slice())?;
            by_full_name.insert(rec.full_name.as_str(), id)?;
            by_https.insert(rec.https_url.as_str(), id)?;
            let ts = rec.pushed_at.unwrap_or(i64::MIN);
            by_pushed.insert((ts, id), ())?;
            written += 1;
        }
        Ok(written)
    }

    fn mutate_repo<F>(&self, repo_id: i64, f: F) -> Result<()>
    where
        F: FnOnce(&mut Repo),
    {
        let id = id_to_u64(repo_id);
        let mut repos = self.txn.open_table(REPOS)?;
        let Some(g) = repos.get(id)? else {
            return Ok(());
        };
        let mut r: Repo = serde_json::from_slice(g.value())?;
        drop(g);
        f(&mut r);
        let bytes = serde_json::to_vec(&r)?;
        repos.insert(id, bytes.as_slice())?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn next_id(txn: &WriteTransaction, key: &str) -> Result<u64> {
    let mut meta = txn.open_table(META)?;
    let n = meta.get(key)?.map(|g| g.value()).unwrap_or(1);
    meta.insert(key, n + 1)?;
    Ok(n)
}

fn clear_table<K, V>(txn: &WriteTransaction, def: TableDefinition<K, V>) -> Result<()>
where
    K: redb::Key + 'static,
    V: redb::Value + 'static,
{
    let mut t = txn.open_table(def)?;
    t.retain(|_, _| false)?;
    Ok(())
}

fn id_to_u64(id: i64) -> u64 {
    id as u64
}

fn u64_to_id(id: u64) -> i64 {
    id as i64
}

fn order_by_started_at_desc(a: Option<i64>, b: Option<i64>) -> std::cmp::Ordering {
    // ORDER BY started_at DESC NULLS LAST.
    use std::cmp::Ordering;
    match (a, b) {
        (Some(x), Some(y)) => y.cmp(&x),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}
