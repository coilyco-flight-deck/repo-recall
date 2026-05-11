use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;

use tokio::sync::{broadcast, Mutex};

pub mod activity;
pub mod commits;
pub mod db;
pub mod join;
pub mod mcp;
pub mod routes;
pub mod scanner;
pub mod search;
pub mod sessions;
pub mod signals;
pub mod spans;

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
    /// HTML-fragment broadcast for the axum dashboard's `/ws` listener. In
    /// `mcp` subcommand mode there are no listeners, so the broadcast no-ops.
    pub progress_tx: broadcast::Sender<String>,
    pub refresh_lock: Arc<Mutex<()>>,
    pub last_scan: Arc<Mutex<Option<chrono::DateTime<chrono::Utc>>>>,
    /// State of the local `gh` CLI. Updated at startup and re-checked at the
    /// start of each refresh so the banner disappears as soon as the user
    /// installs / logs in.
    pub gh_health: Arc<Mutex<commits::GhHealth>>,
    /// GitHub login of the authenticated user, cached from `gh api user`.
    /// `None` when `gh_health != Ok`. Drives the "awaiting my review" split.
    pub my_gh_login: Arc<Mutex<Option<String>>>,
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
}
