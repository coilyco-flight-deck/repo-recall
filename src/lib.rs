use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;

use tokio::sync::Mutex;

pub mod activity;
pub mod commits;
pub mod db;
pub mod join;
pub mod mcp;
pub mod refresh;
pub mod scanner;
pub mod sessions;

#[derive(Clone)]
pub struct AppState {
    pub db_path: PathBuf,
    pub cwd: PathBuf,
    pub scan_depth: usize,
    pub commits_per_repo: usize,
    pub refresh_interval_secs: u64,
    pub remote_target_limit: usize,
    pub refresh_lock: Arc<Mutex<()>>,
    pub last_scan: Arc<Mutex<Option<chrono::DateTime<chrono::Utc>>>>,
    pub gh_health: Arc<Mutex<commits::GhHealth>>,
    pub my_gh_login: Arc<Mutex<Option<String>>>,
    pub my_git_email: Arc<Mutex<Option<String>>>,
    pub scan_version: Arc<AtomicU64>,
}
