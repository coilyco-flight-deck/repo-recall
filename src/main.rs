use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use miette::{IntoDiagnostic, WrapErr};
use tokio::sync::Mutex;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

use repo_recall::{
    config,
    db::CacheDb,
    display::{mcp, routes},
    search, AppState,
};

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
    tracing_subscriber::registry()
        .with(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,repo_recall=debug")),
        )
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .init();

    // Load layered config (built-in defaults <- ~/.config/repo-recall/config.yaml
    // <- ./repo-recall.yaml <- REPO_RECALL_* env vars). See #145.
    let cfg = config::load();
    config::validate(&cfg);

    let cwd = match cfg.paths.cwd.clone() {
        Some(p) => p,
        None => std::env::current_dir()
            .into_diagnostic()
            .wrap_err("failed to read current working directory")?,
    };
    let cwd = dunce::canonicalize(&cwd).unwrap_or(cwd);

    let port: u16 = cfg.server.port;
    // Default is loopback. Override only when fronted by something that gates
    // access at a different layer (e.g. `tailscale serve` on a tailnet-only
    let host: IpAddr = cfg.server.host.parse().unwrap_or_else(|e| {
        tracing::warn!(
            "config server.host={:?} invalid: {e}; falling back to 127.0.0.1",
            cfg.server.host
        );
        IpAddr::from([127, 0, 0, 1])
    });

    // Default cache directory is per-port so two instances (e.g.
    // launchd-managed on 7777 and a dev binary on 7778) don't share state
    let cache_dir = cfg
        .paths
        .cache_dir
        .clone()
        .unwrap_or_else(|| std::env::temp_dir().join(format!("repo-recall-{port}")));

    tracing::info!("cwd:   {}", cwd.display());
    tracing::info!("cache: {}", cache_dir.display());

    // Underlying errors are `anyhow::Error`, which does not implement
    // `std::error::Error`, so `.into_diagnostic()` won't see them. Render via
    let cache_db = CacheDb::open_in_dir(&cache_dir)
        .map_err(|e| miette::miette!("{e:?}"))
        .wrap_err_with(|| format!("failed to open cache db at {}", cache_dir.display()))?;

    let index_dir = search::default_index_dir();
    tracing::info!("idx: {}", index_dir.display());
    let search_index = search::SearchIndex::open_at(&index_dir)
        .map_err(|e| miette::miette!("{e:?}"))
        .wrap_err_with(|| format!("failed to open search index at {}", index_dir.display()))?;

    let scan_depth: usize = cfg.discovery.scan_depth;
    let commits_per_repo: usize = cfg.discovery.commits_per_repo;

    let github_client = repo_recall::ingest::github::build_client();
    let forgejo_host = std::env::var("REPO_RECALL_FORGEJO_HOST")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "forgejo.coilysiren.me".to_string());
    let forgejo_client = repo_recall::ingest::forgejo::build_client(&forgejo_host);
    // Bounded startup probe so a slow/blocked network never wedges boot.
    // Timeout collapses to `Error` so the banner reflects the failure
    let viewer = match tokio::time::timeout(
        std::time::Duration::from_secs(3),
        github_client.fetch_user(),
    )
    .await
    {
        Ok(state) => state,
        Err(_) => {
            tracing::warn!("github auth probe timed out after 3s; viewer = Error");
            repo_recall::ingest::github::RemoteFetchState::Error("startup probe timed out".into())
        }
    };
    let my_git_email = detect_my_git_email();

    let refresh_interval_secs: u64 = cfg.refresh.interval_secs;
    let remote_target_limit: usize = cfg.ingest.github.remote_target_limit;

    let state = AppState {
        cache_db,
        cwd,
        scan_depth,
        commits_per_repo,
        refresh_interval_secs,
        remote_target_limit,
        refresh_lock: Arc::new(Mutex::new(())),
        last_scan: Arc::new(Mutex::new(None)),
        viewer: Arc::new(Mutex::new(viewer)),
        my_git_email: Arc::new(Mutex::new(my_git_email)),
        scan_version: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        search_index,
        remote_backoff_until: Arc::new(Mutex::new(None)),
        remote_backoff_secs: Arc::new(Mutex::new(0)),
        last_good_remote: Arc::new(Mutex::new(std::collections::HashMap::new())),
        github_client,
        forgejo_client,
        remote_kind_cache: repo_recall::ingest::remote_kind::RemoteKindCache::new(),
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

    // Per-source refresh fan-out (#146). Each ingest source carries its
    // own cadence: `refresh.per_source.<source>.interval_secs` overrides
    use routes::refresh::Source;
    let scheduled: Vec<(Source, u64)> = Source::ALL
        .iter()
        .filter_map(|s| {
            let secs = cfg.refresh.interval_for(s.name());
            (secs > 0).then_some((*s, secs))
        })
        .collect();
    if let Some(base_tick) = scheduled.iter().map(|(_, secs)| *secs).min() {
        for (s, secs) in &scheduled {
            tracing::info!("refresh source {}: every {secs}s", s.name());
        }
        let state = state.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(std::time::Duration::from_secs(base_tick));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            ticker.tick().await; // skip first immediate tick — initial scan covers it
            loop {
                ticker.tick().await;
                let now = chrono::Utc::now().timestamp();
                // A source is due when it has never run, or its interval
                // has elapsed since its last completion watermark.
                let due: Vec<Source> = scheduled
                    .iter()
                    .filter(
                        |(s, secs)| match state.cache_db.refresh_watermark(s.name()) {
                            Ok(Some(last)) => now - last >= *secs as i64,
                            Ok(None) => true,
                            Err(e) => {
                                tracing::warn!(
                                    "watermark read failed for {}: {e:?}; treating as due",
                                    s.name()
                                );
                                true
                            }
                        },
                    )
                    .map(|(s, _)| *s)
                    .collect();
                if due.is_empty() {
                    continue;
                }
                if let Err(e) = routes::refresh::run_refresh_for(state.clone(), &due).await {
                    tracing::error!("periodic refresh failed: {e:?}");
                }
            }
        });
    } else {
        tracing::info!("periodic refresh disabled (all source intervals resolve to 0)");
    }

    // Always start MCP. Tolerate dead-stdin: in the brew-services case, stdin
    // is /dev/null so run_stdio returns Ok almost immediately and we keep
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
