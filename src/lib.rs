use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;

use tokio::sync::Mutex;

pub mod config;
pub mod db;
pub mod display;
pub mod ingest;
pub mod process;
pub mod search;
pub mod signals;

#[derive(Clone)]
pub struct AppState {
    pub cache_db: db::CacheDb,
    pub cwd: PathBuf,
    pub scan_depth: usize,
    pub commits_per_repo: usize,
    /// Seconds between periodic background refreshes. `0` disables the
    /// periodic task; the dashboard hides the countdown in that case.
    pub refresh_interval_secs: u64,
    /// Cap on how many GitHub-hosted repos get remote-state queries (CI,
    /// PRs, issues) per refresh — picked as the top-N by most-recent commit
    /// timestamp. Caps `gh` rate consumption on workspaces that grow past
    /// the bucket math. `0` means no cap (every GH-hosted repo is queried).
    pub remote_target_limit: usize,
    pub refresh_lock: Arc<Mutex<()>>,
    pub last_scan: Arc<Mutex<Option<chrono::DateTime<chrono::Utc>>>>,
    /// Categorized state of the GitHub viewer. Updated at startup and
    /// re-checked at the start of each refresh so the banner disappears
    /// as soon as the user logs in. `RemoteFetchState::Ok` carries the
    /// authenticated user's login (drives the "awaiting my review"
    /// split); the other variants drive the banner.
    pub viewer: Arc<Mutex<ingest::github::RemoteFetchState<ingest::github::AuthedUser>>>,
    /// Viewer's git email (from `git config --global user.email`), used as
    /// the default author for `?author=me` filtering. Fallback when
    /// `REPO_RECALL_AUTHOR` isn't set.
    pub my_git_email: Arc<Mutex<Option<String>>>,
    /// Monotonic counter incremented at the end of every successful refresh.
    /// Drives the `scan_version` field in JSON responses + the `ETag` header
    /// on cacheable endpoints, so a polling orchestrator can short-circuit
    /// with `304 Not Modified` between scans.
    pub scan_version: Arc<AtomicU64>,
    /// Tantivy full-text index, dual-written alongside the SQLite
    /// `search_idx` virtual table on every refresh. Reader still flows
    /// through SQLite until the redb migration's step 3 flips it.
    pub search_index: search::SearchIndex,
    /// In-memory store of in-flight autonomous-dispatch sessions. See
    /// `display::mcp::dispatch`. Dropped on restart by design.
    pub dispatch_sessions: display::mcp::dispatch::DispatchSessions,
    /// Minimum seconds between labeled-issue GraphQL ingest passes. Sourced
    /// from `refresh.per_source.github_remote_labeled` (default 3600s). The
    /// labeled-issue ingest is the only sanctioned GraphQL call site, and
    /// the secondary rate limit is shared - gate it explicitly here so
    /// `interval_secs` can stay aggressive without burning the budget.
    pub labeled_ingest_interval_secs: u64,
    /// Last time `ingest_labeled_issues` actually ran (vs being gated).
    /// In-memory; resets on process start, which is fine - the first refresh
    /// after boot should always pull fresh labeled state.
    pub last_labeled_ingest: Arc<Mutex<Option<chrono::DateTime<chrono::Utc>>>>,
    /// Cutoff (in seconds) past which an open structural-context ask
    /// becomes a `stale_ask` action-required signal. Sourced from
    /// `signals.stale_ask_days` (default 7 days) at startup. Replaces an
    /// earlier env-only path that bypassed `Config` entirely.
    pub stale_ask_threshold_secs: i64,
    /// When set, the remote-state pass is skipped until this instant.
    /// Populated when a refresh pass observes a `RateLimited` from any
    /// gh fetcher: the next pass takes the larger of the parsed
    /// `Retry-After` and the current `remote_backoff_secs`. Cleared
    /// (set to `None`) on the first successful pass after the cooldown.
    pub remote_backoff_until: Arc<Mutex<Option<chrono::DateTime<chrono::Utc>>>>,
    /// Current per-step backoff in seconds, doubled on each
    /// rate-limit-tagged pass (clamped to [`REMOTE_BACKOFF_MIN_SECS`,
    /// `REMOTE_BACKOFF_MAX_SECS`]) and reset to 0 on the first
    /// successful pass. Independent of the parsed Retry-After: the
    /// effective sleep is `max(retry_after, backoff_secs)`.
    pub remote_backoff_secs: Arc<Mutex<u64>>,
    /// In-memory shadow of the last-good remote-state per repo. Each
    /// `CachedRemoteState` survives the per-refresh `wipe()` call on
    /// the cache: when a per-repo gh fetch is rate-limited or returns
    /// nothing, the writer falls back to this shadow so a single bad
    /// pass does not blank the entire dashboard. Cleared on process
    /// restart by design (the cache itself rebuilds from disk).
    pub last_good_remote: Arc<Mutex<std::collections::HashMap<i64, CachedRemoteState>>>,
    /// GitHub API client. Either an [`ingest::github::OctocrabClient`]
    /// sourced from `gh auth token` (production), an anonymous octocrab
    /// when no token is available (every method returns
    /// `RemoteFetchState::Unconfigured`), or a
    /// [`ingest::github::FixturesClient`] when
    /// `REPO_RECALL_GITHUB_FIXTURES_DIR` is set (`make watch-fixtures`
    /// + unit tests). #173 step 1.
    pub github_client: Arc<dyn ingest::github::GithubClient>,
}

/// One repo's last-good remote-state, captured on the most recent
/// successful gh fetch. The Vec fields shadow `RemoteSnapshot`'s typed
/// payloads; the wall-clock timestamp powers a future "stale by Xm"
/// freshness pill (issue #169 follow-up).
#[derive(Clone)]
pub struct CachedRemoteState {
    pub ci: Option<String>,
    pub prs: Option<ingest::git::log::PrCounts>,
    pub issues: Option<ingest::git::log::IssueCounts>,
    pub deploy: Option<(String, ingest::git::log::DeployHealth)>,
    pub pr_records: Vec<ingest::github::PrRecordInput>,
    pub issue_records: Vec<ingest::github::IssueRecordInput>,
    pub ci_runs: Vec<ingest::github::CiRunRecordInput>,
    pub captured_at: chrono::DateTime<chrono::Utc>,
}

/// Lower bound for the exponential remote-state backoff. First
/// rate-limit hit stalls the next pass by at least 5 minutes, even if
/// gh did not return a Retry-After header.
pub const REMOTE_BACKOFF_MIN_SECS: u64 = 300;

/// Upper bound for the exponential remote-state backoff. Tracks the
/// REST primary-rate-limit reset window so a sustained block does not
/// cause arbitrary stalling beyond what the budget actually requires.
pub const REMOTE_BACKOFF_MAX_SECS: u64 = 3600;
