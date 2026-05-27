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
    pub remote_target_limit: usize,
    pub refresh_lock: Arc<Mutex<()>>,
    pub last_scan: Arc<Mutex<Option<chrono::DateTime<chrono::Utc>>>>,
    /// Categorized state of the GitHub viewer. Updated at startup and
    /// re-checked at the start of each refresh so the banner disappears
    pub viewer: Arc<Mutex<ingest::github::RemoteFetchState<ingest::github::AuthedUser>>>,
    /// Viewer's git email (from `git config --global user.email`), used as
    /// the default author for `?author=me` filtering. Fallback when
    pub my_git_email: Arc<Mutex<Option<String>>>,
    /// Monotonic counter incremented at the end of every successful refresh.
    /// Drives the `scan_version` field in JSON responses + the `ETag` header
    pub scan_version: Arc<AtomicU64>,
    /// Tantivy full-text index, dual-written alongside the SQLite
    /// `search_idx` virtual table on every refresh. Reader still flows
    pub search_index: search::SearchIndex,
    /// When set, the remote-state pass is skipped until this instant.
    /// Populated when a refresh pass observes a `RateLimited` from any
    pub remote_backoff_until: Arc<Mutex<Option<chrono::DateTime<chrono::Utc>>>>,
    /// Current per-step backoff in seconds, doubled on each
    /// rate-limit-tagged pass (clamped to [`REMOTE_BACKOFF_MIN_SECS`,
    pub remote_backoff_secs: Arc<Mutex<u64>>,
    /// In-memory shadow of the last-good remote-state per repo. Each
    /// `CachedRemoteState` survives the per-refresh `wipe()` call on
    pub last_good_remote: Arc<Mutex<std::collections::HashMap<i64, CachedRemoteState>>>,
    /// GitHub API client. Either an [`ingest::github::OctocrabClient`]
    /// sourced from `gh auth token` (production), an anonymous octocrab
    pub github_client: Arc<dyn ingest::github::GithubClient>,
    /// Forgejo API client (#91). See docs/forgejo-dispatch.md.
    pub forgejo_client: Arc<dyn ingest::github::GithubClient>,
    /// Per-host kind cache (#91). See docs/forgejo-dispatch.md.
    pub remote_kind_cache: ingest::remote_kind::RemoteKindCache,
}

/// One repo's last-good remote-state, captured on the most recent
/// successful gh fetch. The Vec fields shadow `RemoteSnapshot`'s typed
#[derive(Clone)]
pub struct CachedRemoteState {
    pub prs: Option<ingest::git::log::PrCounts>,
    pub issues: Option<ingest::git::log::IssueCounts>,
    pub deploy: Option<(String, ingest::git::log::DeployHealth)>,
    pub pr_records: Vec<ingest::github::PrRecordInput>,
    pub issue_records: Vec<ingest::github::IssueRecordInput>,
    pub milestones: Vec<ingest::github::MilestoneInput>,
    pub captured_at: chrono::DateTime<chrono::Utc>,
}

/// Lower bound for the exponential remote-state backoff. First
/// rate-limit hit stalls the next pass by at least 5 minutes, even if
pub const REMOTE_BACKOFF_MIN_SECS: u64 = 300;

/// Upper bound for the exponential remote-state backoff. Tracks the
/// REST primary-rate-limit reset window so a sustained block does not
pub const REMOTE_BACKOFF_MAX_SECS: u64 = 3600;
