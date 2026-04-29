use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::Mutex;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

use repo_recall::{commits, db, mcp, refresh, AppState};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("repo-recall {}", env!("REPO_RECALL_VERSION"));
        return Ok(());
    }
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_help();
        return Ok(());
    }

    let _ = dotenvy::dotenv();

    // MCP servers must keep stdout pristine for JSON-RPC framing. Send all
    // tracing to stderr.
    tracing_subscriber::registry()
        .with(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,repo_recall=debug")),
        )
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .init();

    let cwd = match std::env::var_os("REPO_RECALL_CWD") {
        Some(p) => PathBuf::from(p),
        None => std::env::current_dir()?,
    };
    let cwd = dunce::canonicalize(&cwd).unwrap_or(cwd);

    let db_path = std::env::var("REPO_RECALL_DB")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir().join("repo-recall-mcp.sqlite"));

    tracing::info!("cwd: {}", cwd.display());
    tracing::info!("db:  {}", db_path.display());

    db::init(&db_path)?;

    let scan_depth: usize = env_usize("REPO_RECALL_DEPTH", 4);
    let commits_per_repo: usize = env_usize("REPO_RECALL_COMMITS_PER_REPO", 500);
    let refresh_interval_secs: u64 = env_u64("REPO_RECALL_REFRESH_INTERVAL_SECS", 150);
    let remote_target_limit: usize = env_usize("REPO_RECALL_REMOTE_TARGET_LIMIT", 25);

    let gh_health = commits::gh_health();
    let my_gh_login = if gh_health == commits::GhHealth::Ok {
        commits::my_gh_login()
    } else {
        None
    };
    let my_git_email = detect_my_git_email();

    let state = AppState {
        db_path,
        cwd,
        scan_depth,
        commits_per_repo,
        refresh_interval_secs,
        remote_target_limit,
        refresh_lock: Arc::new(Mutex::new(())),
        last_scan: Arc::new(Mutex::new(None)),
        gh_health: Arc::new(Mutex::new(gh_health)),
        my_gh_login: Arc::new(Mutex::new(my_gh_login)),
        my_git_email: Arc::new(Mutex::new(my_git_email)),
        scan_version: Arc::new(std::sync::atomic::AtomicU64::new(0)),
    };

    // Initial scan in the background so the first tool call has data.
    {
        let state = state.clone();
        tokio::spawn(async move {
            if let Err(e) = refresh::run_refresh(state).await {
                tracing::error!("initial refresh failed: {e:?}");
            }
        });
    }

    if refresh_interval_secs > 0 {
        tracing::info!("periodic refresh: every {refresh_interval_secs}s");
        let state = state.clone();
        tokio::spawn(async move {
            let mut ticker =
                tokio::time::interval(std::time::Duration::from_secs(refresh_interval_secs));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            ticker.tick().await; // skip first immediate tick
            loop {
                ticker.tick().await;
                if let Err(e) = refresh::run_refresh(state.clone()).await {
                    tracing::error!("periodic refresh failed: {e:?}");
                }
            }
        });
    } else {
        tracing::info!("periodic refresh disabled");
    }

    tracing::info!("starting MCP server on stdio");
    mcp::run_stdio(state).await?;
    Ok(())
}

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

fn print_help() {
    println!(
        "repo-recall {ver}
Local MCP server that indexes Claude Code session history against your repos.
Connects via stdio. Add to your MCP host config to use.

Usage:
  repo-recall              run as MCP stdio server (default)
  repo-recall --version    print version and exit
  repo-recall --help       print this help and exit

Config via env vars (or a .env file in cwd). See AGENTS.md for the full list.
Common: REPO_RECALL_CWD, REPO_RECALL_DEPTH, REPO_RECALL_DB,
REPO_RECALL_REFRESH_INTERVAL_SECS.
",
        ver = env!("REPO_RECALL_VERSION"),
    );
}

fn detect_my_git_email() -> Option<String> {
    if let Ok(email) = std::env::var("REPO_RECALL_AUTHOR") {
        if !email.is_empty() && email != "all" {
            return Some(email);
        }
    }
    let out = std::process::Command::new("git")
        .args(["config", "--global", "user.email"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}
