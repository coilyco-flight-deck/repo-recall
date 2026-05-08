use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;

use miette::{IntoDiagnostic, WrapErr};
use tokio::sync::{broadcast, Mutex};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

use repo_recall::{commits, db::CacheDb, mcp, routes, search, state::StateDb, AppState};

#[tokio::main]
async fn main() -> miette::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("repo-recall {}", env!("REPO_RECALL_VERSION"));
        return Ok(());
    }
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_help();
        return Ok(());
    }

    // Load .env from cwd (or repo root when launched via cargo run) before
    // reading any env vars. Missing .env is not an error.
    let _ = dotenvy::dotenv();

    // Single binary, both surfaces. The MCP server is purely additive.
    // tracing-subscriber writer is stderr unconditionally because the MCP
    // stdio transport reserves stdout for JSON-RPC framing.
    tracing_subscriber::registry()
        .with(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,repo_recall=debug")),
        )
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .init();

    let cwd = match std::env::var_os("REPO_RECALL_CWD") {
        Some(p) => PathBuf::from(p),
        None => std::env::current_dir()
            .into_diagnostic()
            .wrap_err("failed to read current working directory")?,
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

    // Default cache directory is per-port so two instances (e.g.
    // launchd-managed on 7777 and a dev binary on 7778) don't share state
    // and wipe each other's tables during their periodic refreshes. Override
    // with REPO_RECALL_CACHE_DIR. The cache file inside is `cache.redb` and
    // is recreated on every startup (wipe-on-restart).
    let cache_dir = std::env::var("REPO_RECALL_CACHE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir().join(format!("repo-recall-{port}")));

    tracing::info!("cwd:   {}", cwd.display());
    tracing::info!("cache: {}", cache_dir.display());

    // Publish cwd so the layout header can render it without threading state
    // through every `page(..)` call site. Set-once; ignored if already set
    // (e.g. across hot reloads in tests).
    let _ = repo_recall::routes::templates::SCAN_CWD.set(cwd.clone());

    // Underlying errors are `anyhow::Error`, which does not implement
    // `std::error::Error`, so `.into_diagnostic()` won't see them. Render via
    // `miette::miette!` and tack on a wrap_err for the labeled "failed to
    // bootstrap because X" line.
    let cache_db = CacheDb::open_in_dir(&cache_dir)
        .map_err(|e| miette::miette!("{e:?}"))
        .wrap_err_with(|| format!("failed to open cache db at {}", cache_dir.display()))?;

    let index_dir = search::default_index_dir();
    tracing::info!("idx: {}", index_dir.display());
    let search_index = search::SearchIndex::open_at(&index_dir)
        .map_err(|e| miette::miette!("{e:?}"))
        .wrap_err_with(|| format!("failed to open search index at {}", index_dir.display()))?;

    let state_db = StateDb::open_default()
        .map_err(|e| miette::miette!("{e:?}"))
        .wrap_err("failed to open persistent state db")?;
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
        cache_db,
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
        search_index,
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
            ticker.tick().await; // skip first immediate tick — initial scan covers it
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

    // Always start MCP. Tolerate dead-stdin: in the brew-services case, stdin
    // is /dev/null so run_stdio returns Ok almost immediately and we keep
    // running axum. In the Claude-Desktop-spawned case, stdin is a pipe from
    // the host and run_stdio holds it open for the session's life.
    let mcp_state = state.clone();
    let mcp_handle = tokio::spawn(async move {
        if let Err(e) = mcp::run_stdio(mcp_state).await {
            tracing::warn!("mcp stdio server exited: {e:?}");
        }
    });

    // Always try to bind axum. If the port is already in use (e.g. a brew
    // service is already serving), log and fall back to MCP-only.
    let addr: SocketAddr = SocketAddr::new(host, port);
    match tokio::net::TcpListener::bind(addr).await {
        Ok(listener) => {
            tracing::info!("listening on http://{}", addr);
            let app = routes::router(state.clone());
            axum::serve(listener, app)
                .await
                .into_diagnostic()
                .wrap_err("axum serve loop failed")?;
        }
        Err(e) => {
            tracing::warn!("could not bind {addr}: {e}. Skipping axum, running MCP only.");
            // MCP becomes the only foreground task. Wait on it forever (or
            // until stdin EOFs and the host disconnects).
            let _ = mcp_handle.await;
        }
    }
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

Always runs both an axum dashboard (HTTP) and an MCP App server (stdio) in
one process. The dashboard binds $REPO_RECALL_HOST:$REPO_RECALL_PORT (default
127.0.0.1:7777). The MCP server reads JSON-RPC from stdin and writes to
stdout, so wire it into your host's mcpServers config with the bare command.

If the HTTP port is already in use (e.g. another instance is already running
under brew services), this instance falls back to MCP-only.

Usage:
  repo-recall              start both (default)
  repo-recall --version    print version and exit
  repo-recall --help       print this help and exit

Config is via env vars (or a .env file in cwd). See the README for the full
list. Common ones: REPO_RECALL_PORT, REPO_RECALL_HOST, REPO_RECALL_CWD,
REPO_RECALL_DEPTH, REPO_RECALL_CACHE_DIR.
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
