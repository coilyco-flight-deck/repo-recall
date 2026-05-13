//! MCP App server. Tools expose repo-recall's data layer to MCP hosts.
//!
//! Nine tools:
//!
//! - `recall_dashboard` — repo list + action-required + counts.
//! - `recall_repo` — single repo detail.
//! - `recall_session` — single session detail.
//! - `recall_search` — unified search.
//! - `recall_action_required` — thin orchestrator slice.
//! - `recall_ticket_history` — sessions + commits touching one issue.
//! - `recall_autonomy_metrics` — AFK success rate from dispatch ledger.
//! - `recall_record_dispatch` — emit a new write-once dispatch artifact.
//! - `recall_refresh` — trigger a rescan.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use axum::Router;
use pmcp::server::axum_router::{router_with_config, AllowedOrigins, RouterConfig};
use pmcp::{Server, ServerCapabilities, TypedTool};
use tokio::sync::Mutex;

use crate::AppState;

mod tools;

/// Build the MCP `Server` (tools) without binding any transport. Shared by
/// stdio and HTTP entrypoints.
pub fn build_server(state: AppState) -> anyhow::Result<Server> {
    let dashboard = {
        let state = state.clone();
        TypedTool::new("recall_dashboard", move |args, extra| {
            let s = state.clone();
            Box::pin(tools::dashboard(s, args, extra))
        })
        .with_description(
            "Ranked list of repos discovered on disk with their action-required signals, \
             session counts, and 30-day activity.",
        )
    };

    let repo = {
        let state = state.clone();
        TypedTool::new("recall_repo", move |args, extra| {
            let s = state.clone();
            Box::pin(tools::repo(s, args, extra))
        })
        .with_description(
            "Detail view for a single repo: hottest files by churn, sessions that touched it, \
             recent commits, remote state.",
        )
    };

    let session = {
        let state = state.clone();
        TypedTool::new("recall_session", move |args, extra| {
            let s = state.clone();
            Box::pin(tools::session(s, args, extra))
        })
        .with_description(
            "Detail view for a single Claude Code session: metadata, repos it touched, summary.",
        )
    };

    let search = {
        let state = state.clone();
        TypedTool::new("recall_search", move |args, extra| {
            let s = state.clone();
            Box::pin(tools::search(s, args, extra))
        })
        .with_description(
            "Unified search across repos, sessions, and commits. Returns partitioned hits.",
        )
    };

    let action_required = {
        let state = state.clone();
        TypedTool::new("recall_action_required", move |args, extra| {
            let s = state.clone();
            Box::pin(tools::action_required(s, args, extra))
        })
        .with_description(
            "Thin orchestrator slice: only the repos with at least one action-required signal \
             (failing CI, dirty tree, in-progress git op, detached HEAD, awaiting review, \
             assigned issue, deploy failing/stale).",
        )
    };

    let ticket_history = {
        let state = state.clone();
        TypedTool::new("recall_ticket_history", move |args, extra| {
            let s = state.clone();
            Box::pin(tools::ticket_history(s, args, extra))
        })
        .with_description(
            "Sessions and commits in repo-recall's cache that reference a given issue \
             in a given repo. Used by recall-dispatch to ground per-ticket context in \
             real prior work. Returns empty arrays when the issue is unindexed.",
        )
    };

    let autonomy_metrics = {
        let state = state.clone();
        TypedTool::new("recall_autonomy_metrics", move |args, extra| {
            let s = state.clone();
            Box::pin(tools::autonomy_metrics(s, args, extra))
        })
        .with_description(
            "AFK success rate aggregated from closed repo-dispatch tracking issues. \
             Classifies closures into success / abandon / block by joining against \
             commit issue-refs. Returns overall + per-repo rates with sample sizes.",
        )
    };

    let record_dispatch = {
        let state = state.clone();
        TypedTool::new("recall_record_dispatch", move |args, extra| {
            let s = state.clone();
            Box::pin(tools::record_dispatch(s, args, extra))
        })
        .with_description(
            "Emit a new write-once dispatch artifact. Writes both the in-repo \
             docs/repo-dispatch/<slug>.md (canonical, gets committed by the caller) \
             and ~/.repo-recall/dispatch/<repo>/<slug>.md (pollable mirror for \
             sub-agent runners). Refuses to overwrite an existing slug.",
        )
    };

    let open_structural_asks = {
        let state = state.clone();
        TypedTool::new("recall_open_structural_asks", move |args, extra| {
            let s = state.clone();
            Box::pin(tools::open_structural_asks(s, args, extra))
        })
        .with_description(
            "List currently open structural-ask-labeled GitHub issues across the \
             indexed workspace. Read this before drafting a new ask so the planner \
             can refuse to re-ask a question already on the list.",
        )
    };

    let emit_structural_ask = {
        let state = state.clone();
        TypedTool::new("recall_emit_structural_ask", move |args, extra| {
            let s = state.clone();
            Box::pin(tools::emit_structural_ask(s, args, extra))
        })
        .with_description(
            "Draft a structural-context ask. Writes a write-once markdown file \
             under ~/.repo-recall/structural-asks/<slug>.md for review and \
             posting as a structural-ask-labeled issue. Free text is sanitized \
             before write. Refuses to overwrite an existing slug.",
        )
    };

    let emit_agents_drift = {
        let state = state.clone();
        TypedTool::new("recall_emit_agents_drift_proposal", move |args, extra| {
            let s = state.clone();
            Box::pin(tools::emit_agents_drift_proposal(s, args, extra))
        })
        .with_description(
            "Draft an AGENTS.md drift proposal. Writes a write-once markdown file \
             under ~/.repo-recall/agents-drift/<repo>/<slug>.md proposing a new \
             rule for the repo's AGENTS.md, with the supporting dispatches inline. \
             Free text is sanitized before write. Refuses to overwrite an existing \
             slug.",
        )
    };

    let refresh_tool = {
        let state = state.clone();
        TypedTool::new("recall_refresh", move |args, extra| {
            let s = state.clone();
            Box::pin(tools::refresh(s, args, extra))
        })
        .with_description(
            "Trigger a fresh scan of repos, sessions, commits, and remote state. Awaits \
             completion. Coalesces with any in-flight scan.",
        )
    };

    let server = Server::builder()
        .name("repo-recall")
        .version(env!("REPO_RECALL_VERSION"))
        .capabilities(ServerCapabilities::default())
        .tool("recall_dashboard", dashboard)
        .tool("recall_repo", repo)
        .tool("recall_session", session)
        .tool("recall_search", search)
        .tool("recall_action_required", action_required)
        .tool("recall_ticket_history", ticket_history)
        .tool("recall_autonomy_metrics", autonomy_metrics)
        .tool("recall_record_dispatch", record_dispatch)
        .tool("recall_open_structural_asks", open_structural_asks)
        .tool("recall_emit_structural_ask", emit_structural_ask)
        .tool("recall_emit_agents_drift_proposal", emit_agents_drift)
        .tool("recall_refresh", refresh_tool)
        .build()
        .map_err(|e| anyhow::anyhow!("Server::build failed: {e:?}"))?;

    tracing::info!(
        "MCP server ready: scan_version={}",
        state.scan_version.load(Ordering::Acquire)
    );
    Ok(server)
}

/// Run the MCP server over stdio. Used by the Claude-Desktop spawn case.
pub async fn run_stdio(state: AppState) -> anyhow::Result<()> {
    let server = build_server(state)?;
    server
        .run_stdio()
        .await
        .map_err(|e| anyhow::anyhow!("server stdio loop failed: {e:?}"))?;
    Ok(())
}

/// Build a streamable-HTTP MCP router. Mount under a path prefix to expose
/// `POST <prefix>` (JSON-RPC) and `GET <prefix>` (SSE) per the MCP spec.
///
/// `REPO_RECALL_MCP_ORIGINS` is a comma-separated list of additional origin
/// URLs (`scheme://host[:port]`) to allow past pmcp's DNS-rebinding check.
/// Loopback aliases are always allowed; the env var is for non-loopback
/// hostnames a reverse proxy might forward (e.g. `https://repo-recall.localhost`).
pub fn http_router(state: AppState) -> anyhow::Result<Router> {
    let server = build_server(state)?;
    let mut origins: Vec<String> = vec![
        "http://localhost".into(),
        "http://127.0.0.1".into(),
        "http://[::1]".into(),
        "https://repo-recall.localhost".into(),
    ];
    if let Ok(extra) = std::env::var("REPO_RECALL_MCP_ORIGINS") {
        origins.extend(
            extra
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
        );
    }
    let cfg = RouterConfig {
        allowed_origins: Some(AllowedOrigins::explicit(origins)),
        ..Default::default()
    };
    Ok(router_with_config(Arc::new(Mutex::new(server)), cfg))
}
