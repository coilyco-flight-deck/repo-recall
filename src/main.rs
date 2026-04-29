use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use tokio::sync::{broadcast, Mutex};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

use repo_recall::{commits, db, mcp, routes, state::StateDb, AppState};

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

    // Subcommands. Default invocation runs the axum dashboard (existing
    // behavior). `repo-recall mcp` runs an MCP stdio server on top of the
    // same data layer; intended to be wired into a host's mcpServers config.
    let mode = match args.first().map(String::as_str) {
        Some("mcp") => Mode::Mcp,
        Some("serve") | None => Mode::Serve,
        Some(other) => {
            eprintln!("repo-recall: unknown subcommand `{other}` (try `--help`)");
            std::process::exit(2);
        }
    };

    // Load .env from cwd (or repo root when launched via cargo run) before
    // reading any env vars. Missing .env is not an error.
    let _ = dotenvy::dotenv();

    init_tracing(mode);

    let cwd = match std::env::var_os("REPO_RECALL_CWD") {
        Some(p) => PathBuf::from(p),
        None => std::env::current_dir()?,
    };
    let cwd = dunce::canonicalize(&cwd).unwrap_or(cwd);

    let port: u16 = std::env::var("REPO_RECALL_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(7777);

    // Default is loopback. Override only when fronted by something that gates
    // access at a different layer (e.g. `tailscale serve` on a tailnet-only
    // host). Setting this to a non-loopback address on a shared or public-facing
    // box would expose session metadata.
    let host: IpAddr = std::env::var("REPO_RECALL_HOST")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| IpAddr::from([127, 0, 0, 1]));

    // Default DB path is per-port for axum mode (so two instances on
    // different ports don't share state) and a separate file for MCP mode
    // (so they don't fight over the same sqlite when both run side-by-side).
    let db_path = std::env::var("REPO_RECALL_DB")
        .map(PathBuf::from)
        .unwrap_or_else(|_| match mode {
            Mode::Serve => std::env::temp_dir().join(format!("repo-recall-{port}.sqlite")),
            Mode::Mcp => std::env::temp_dir().join("repo-recall-mcp.sqlite"),
        });

    tracing::info!("mode: {:?}", mode);
    tracing::info!("cwd:  {}", cwd.display());
    tracing::info!("db:   {}", db_path.display());

    db::init(&db_path)?;

    let state_db = StateDb::open_default()?;
    if let Err(e) = state_db.get_or_init_vapid() {
        tracing::warn!("VAPID keypair init failed; push notifications disabled: {e:?}");
    }

    let (progress_tx, _) = broadcast::channel::<String>(128);

    let scan_depth: usize = env_usize("REPO_RECALL_DEPTH", 4);
    let commits_per_repo: usize = env_usize("REPO_RECALL_COMMITS_PER_REPO", 500);

    let gh_health = commits::gh_health();
    let my_gh_login = if gh_health == commits::GhHealth::Ok {
        commits::my_gh_login()
    } else {
        None
    };
    let my_git_email = detect_my_git_email();

    let refresh_interval_secs: u64 = env_u64("REPO_RECALL_REFRESH_INTERVAL_SECS", 150);
    let remote_target_limit: usize = env_usize("REPO_RECALL_REMOTE_TARGET_LIMIT", 25);

    // Public demo: turns the layout banner on and 403s host-mutating endpoints.
    // Off by default; only set to `true` for the public Docker image.
    let demo_mode = matches!(
        std::env::var("REPO_RECALL_DEMO").as_deref(),
        Ok("true") | Ok("TRUE") | Ok("True")
    );
    if demo_mode {
        tracing::info!("REPO_RECALL_DEMO=true: banner on, mutating endpoints disabled");
    }

    let state = AppState {
        db_path,
        cwd,
        scan_depth,
        commits_per_repo,
        refresh_interval_secs,
        remote_target_limit,
        progress_tx,
        refresh_lock: Arc::new(Mutex::new(())),
        last_scan: Arc::new(Mutex::new(None)),
        gh_health: Arc::new(Mutex::new(gh_health)),
        my_gh_login: Arc::new(Mutex::new(my_gh_login)),
        my_git_email: Arc::new(Mutex::new(my_git_email)),
        scan_version: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        state_db,
        demo_mode,
    };

    // Initial scan in the background so the dashboard / first MCP tool call
    // has data by the time the user finishes typing the URL.
    {
        let state = state.clone();
        tokio::spawn(async move {
            if let Err(e) = routes::refresh::run_refresh(state).await {
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
            ticker.tick().await; // skip immediate first tick — initial scan covers it
            loop {
                ticker.tick().await;
                if let Err(e) = routes::refresh::run_refresh(state.clone()).await {
                    tracing::error!("periodic refresh failed: {e:?}");
                }
            }
        });
    } else {
        tracing::info!("periodic refresh disabled (REPO_RECALL_REFRESH_INTERVAL_SECS=0)");
    }

    match mode {
        Mode::Serve => run_axum(state, host, port).await,
        Mode::Mcp => mcp::run_stdio(state).await,
    }
}

#[derive(Debug, Clone, Copy)]
enum Mode {
    Serve,
    Mcp,
}

/// Tracing init. axum mode writes to stdout (or wherever the user redirects);
/// MCP mode MUST keep stdout pristine for JSON-RPC framing, so it goes to
/// stderr.
fn init_tracing(mode: Mode) {
    let env = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,repo_recall=debug"));
    let registry = tracing_subscriber::registry().with(env);
    match mode {
        Mode::Serve => registry.with(tracing_subscriber::fmt::layer()).init(),
        Mode::Mcp => registry
            .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
            .init(),
    }
}

async fn run_axum(state: AppState, host: IpAddr, port: u16) -> anyhow::Result<()> {
    let app: Router = routes::router(state.clone());
    let addr: SocketAddr = SocketAddr::new(host, port);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("listening on http://{}", addr);
    axum::serve(listener, app).await?;
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
Local dev dashboard that indexes Claude Code session history against your repos.

Usage:
  repo-recall [serve]      run the axum dashboard server (default).
                           Binds $REPO_RECALL_HOST:$REPO_RECALL_PORT (default 127.0.0.1:7777).
  repo-recall mcp          run as an MCP stdio server. Wire into your host's
                           mcpServers config (Claude Desktop, ChatGPT, mcp-preview).
  repo-recall --version    print version and exit
  repo-recall --help       print this help and exit

Config is via env vars (or a .env file in cwd). See the README for the full list.
Common ones: REPO_RECALL_PORT, REPO_RECALL_HOST, REPO_RECALL_CWD, REPO_RECALL_DEPTH.
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
